//! Dataset-analytics IPC commands — slice 4 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use
//! dataset_analytics::*;`), so `lib.rs`'s invoke_handler still names `commands::get_dataset_stats`
//! and the frontend invokes are untouched. Same functions, only relocated.
//!
//! Each is a whole-dataset read/compute (stats, quality, validation, the intelligence report, the
//! conformal certificate, annotation-drift, label-quality lift) run via `run_blocking` so a large
//! library never freezes the UI thread.

use super::{run_blocking, RATE_LIMITER};
use crate::ipc_contract::{CommandErrorV1, SuggestedActionV1};
use crate::{quality, stats, AppState};
use tauri::State;

fn analytics_rate_limited(message: &str) -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", message, true).suggested(SuggestedActionV1::Retry)
}

fn analytics_failed(code: &str, message: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, false).suggested(SuggestedActionV1::OpenHealth)
}

fn validate_certificate_request(target_error: f64, confidence_level: f64) -> Result<(), CommandErrorV1> {
    if !target_error.is_finite()
        || target_error <= 0.0
        || target_error > 1.0
        || !confidence_level.is_finite()
        || confidence_level <= 0.0
        || confidence_level >= 1.0
    {
        return Err(CommandErrorV1::new(
            "INVALID_CERTIFICATE_PARAMETERS",
            "Certificate error and confidence values must be valid probabilities.",
            false,
        ));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_dataset_stats(state: State<'_, AppState>) -> Result<stats::DatasetStats, CommandErrorV1> {
    RATE_LIMITER
        .check("get_dataset_stats")
        .map_err(|_| analytics_rate_limited("The dataset summary is busy. Retry in a moment."))?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        stats::compute_stats(&db).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| {
        analytics_failed(
            "DATASET_STATS_FAILED",
            "The dataset summary could not be computed. Open Health for recovery options.",
        )
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_dataset_quality(state: State<'_, AppState>) -> Result<quality::DatasetQuality, CommandErrorV1> {
    RATE_LIMITER
        .check("get_dataset_quality")
        .map_err(|_| analytics_rate_limited("The quality audit is busy. Retry in a moment."))?;
    let settings = state.lock_settings().clone(); // snapshot before moving into the blocking task
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        quality::compute_quality_with_settings(&db, &settings).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| {
        analytics_failed(
            "DATASET_QUALITY_FAILED",
            "The dataset quality audit could not be computed. Open Health for recovery options.",
        )
    })
}

/// Library-wide training grade + the reasons behind it, for the Insights readiness verdict.
///
/// The dashboard's "ready / not ready" MUST agree with what an export would actually write, so this
/// reuses `quality::training_grade_breakdown` — the same `training_grade_for_segment` the export
/// gates on — rather than approximating readiness from the verified count. Those two disagree
/// exactly when it matters most: a library can be 100% human-verified and still export zero rows
/// (e.g. every clip carries `energy_heuristic_alignment` because no word aligner is installed).
#[tauri::command]
#[specta::specta]
pub async fn get_training_grade_breakdown(
    state: State<'_, AppState>,
) -> Result<quality::TrainingGradeBreakdown, CommandErrorV1> {
    RATE_LIMITER
        .check("get_training_grade_breakdown")
        .map_err(|_| analytics_rate_limited("The training-readiness summary is busy. Retry in a moment."))?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        // P1.3: folded from a stream. The breakdown is corpus-wide BY DESIGN — skipping rows would be
        // wrong, not slow — so the fix is not a WHERE clause, and it is deliberately not a SQL
        // reimplementation of the grading rule either: `training_grade_for_segment` stays the one
        // implementation the export also gates on. Only the row's lifetime shrinks. State is O(distinct
        // reasons); it used to be O(corpus) full records.
        let mut tally = quality::TrainingGradeTally::default();
        db.for_each_segment(None, |seg| tally.push(&seg)).map_err(|e| e.to_string())?;
        Ok(tally.finish())
    })
    .await
    .map_err(|_| {
        analytics_failed(
            "TRAINING_GRADE_FAILED",
            "Training readiness could not be computed. Open Health for recovery options.",
        )
    })
}

#[tauri::command]
#[specta::specta]
pub async fn validate_dataset_cmd(
    state: State<'_, AppState>,
) -> Result<crate::validation::ValidationReport, CommandErrorV1> {
    // Rate-limited like its read siblings: this runs a full-dataset validation scan under the db lock,
    // so an unthrottled webview loop would starve every other DB command.
    RATE_LIMITER
        .check("validate_dataset_cmd")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("validate_dataset_cmd"))?;
    let settings = state.lock_settings().clone(); // snapshot before moving into the blocking task
    let db = state.db_arc();
    let result = run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::validation::validate_dataset_with_settings(&db, &settings).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner dataset-validation command failed: {error}");
        crate::ipc_contract::public_owner_data_error(crate::ipc_contract::OwnerDataOperationV1::ValidateDataset, &error)
    })
}

/// Intelligence read-side: LOOP-0 shadow precision (the C5 go-live evidence) + auto-accept
/// precision (C4) joined against subsequent human decisions.
#[tauri::command]
#[specta::specta]
pub async fn get_intelligence_report(
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::IntelligenceReportV1, CommandErrorV1> {
    RATE_LIMITER
        .check("get_intelligence_report")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("get_intelligence_report"))?;
    let db = state.db_arc();
    let result = run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        let value = db.intelligence_report().map_err(|e| e.to_string())?;
        crate::ipc_contract::decode_intelligence_report(value)
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner intelligence-report command failed: {error}");
        crate::ipc_contract::public_owner_analysis_error(
            crate::ipc_contract::OwnerAnalysisOperationV1::IntelligenceReport,
            &error,
        )
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_dataset_certificate(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
) -> Result<crate::quality::conformal::ConformalCertificate, CommandErrorV1> {
    RATE_LIMITER
        .check("get_dataset_certificate")
        .map_err(|_| analytics_rate_limited("The dataset certificate is busy. Retry in a moment."))?;
    validate_certificate_request(target_error, confidence_level)?;
    let db = state.db_arc();
    run_blocking(move || {
        // P1.3: folded from a stream. See get_training_grade_breakdown — same reasoning, and the
        // membership rules stay in conformal.rs rather than being restated in SQL.
        let tally = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            let mut tally = crate::quality::conformal::ConformalTally::default();
            db.for_each_segment(None, |seg| tally.push(&seg)).map_err(|e| e.to_string())?;
            tally
        };
        Ok(tally.finish(target_error, confidence_level))
    })
    .await
    .map_err(|_| {
        analytics_failed(
            "DATASET_CERTIFICATE_FAILED",
            "The dataset certificate could not be computed. Open Health for recovery options.",
        )
    })
}

/// Measured raw-ASR vs post-jury label-quality lift (M3.1) over human-verified segments.
#[tauri::command]
#[specta::specta]
pub async fn get_label_quality_lift(
    state: State<'_, AppState>,
) -> Result<crate::eval::LabelQualityLift, CommandErrorV1> {
    RATE_LIMITER
        .check("get_label_quality_lift")
        .map_err(|_| analytics_rate_limited("The label-quality analysis is busy. Retry in a moment."))?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        let triples = crate::eval::load_lift_triples(&db).map_err(|e| e.to_string())?;
        Ok(crate::eval::compute_label_quality_lift(&triples, 2000, 1234))
    })
    .await
    .map_err(|_| {
        analytics_failed(
            "LABEL_QUALITY_LIFT_FAILED",
            "Label-quality lift could not be computed. Open Health for recovery options.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytics_failures_are_stable_and_never_forward_internal_details() {
        let error = analytics_failed(
            "DATASET_STATS_FAILED",
            "The dataset summary could not be computed. Open Health for recovery options.",
        );
        let wire = serde_json::to_string(&error).expect("serialize analytics error");
        assert!(wire.contains("DATASET_STATS_FAILED"));
        assert!(!wire.contains("C:\\"));
        assert!(!wire.contains("SQL"));
        assert!(!wire.contains("token"));

        let busy = serde_json::to_value(analytics_rate_limited("Busy. Retry in a moment.")).unwrap();
        assert_eq!(busy["retryable"], true);
        assert_eq!(busy["suggestedAction"], "retry");

        for (target, confidence) in [(0.0, 0.95), (1.1, 0.95), (0.05, 0.0), (0.05, 1.0)] {
            let invalid = validate_certificate_request(target, confidence).expect_err("invalid probability");
            assert_eq!(invalid.code, "INVALID_CERTIFICATE_PARAMETERS");
        }
        validate_certificate_request(0.05, 0.95).expect("normal certificate request");
    }
}
