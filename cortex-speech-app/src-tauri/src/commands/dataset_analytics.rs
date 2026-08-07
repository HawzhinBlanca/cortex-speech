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
use crate::{quality, stats, AppState};
use tauri::State;

#[tauri::command]
pub async fn get_dataset_stats(state: State<'_, AppState>) -> Result<stats::DatasetStats, String> {
    RATE_LIMITER.check("get_dataset_stats")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        stats::compute_stats(&db).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn get_dataset_quality(state: State<'_, AppState>) -> Result<quality::DatasetQuality, String> {
    RATE_LIMITER.check("get_dataset_quality")?;
    let settings = state.lock_settings().clone(); // snapshot before moving into the blocking task
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        quality::compute_quality_with_settings(&db, &settings).map_err(|e| e.to_string())
    })
    .await
}

/// Library-wide training grade + the reasons behind it, for the Insights readiness verdict.
///
/// The dashboard's "ready / not ready" MUST agree with what an export would actually write, so this
/// reuses `quality::training_grade_breakdown` — the same `training_grade_for_segment` the export
/// gates on — rather than approximating readiness from the verified count. Those two disagree
/// exactly when it matters most: a library can be 100% human-verified and still export zero rows
/// (e.g. every clip carries `energy_heuristic_alignment` because no word aligner is installed).
#[tauri::command]
pub async fn get_training_grade_breakdown(
    state: State<'_, AppState>,
) -> Result<quality::TrainingGradeBreakdown, String> {
    RATE_LIMITER.check("get_training_grade_breakdown")?;
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
}

#[tauri::command]
pub async fn validate_dataset_cmd(state: State<'_, AppState>) -> Result<crate::validation::ValidationReport, String> {
    // Rate-limited like its read siblings: this runs a full-dataset validation scan under the db lock,
    // so an unthrottled webview loop would starve every other DB command.
    RATE_LIMITER.check("validate_dataset_cmd")?;
    let settings = state.lock_settings().clone(); // snapshot before moving into the blocking task
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::validation::validate_dataset_with_settings(&db, &settings).map_err(|e| e.to_string())
    })
    .await
}

/// Intelligence read-side: LOOP-0 shadow precision (the C5 go-live evidence) + auto-accept
/// precision (C4) joined against subsequent human decisions.
#[tauri::command]
pub async fn get_intelligence_report(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("get_intelligence_report")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.intelligence_report().map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn get_dataset_certificate(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
) -> Result<crate::quality::conformal::ConformalCertificate, String> {
    RATE_LIMITER.check("get_dataset_certificate")?;
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
}

/// Compute the annotation-drift scorecard for the current dataset: how much human
/// reviewers had to change the raw ASR output (micro WER/CER with bootstrap CIs). Reads
/// the live segments directly — unlike `build_scorecard` it needs no held-out eval run.
#[tauri::command]
pub async fn compute_annotation_drift_scorecard(
    state: State<'_, AppState>,
) -> Result<crate::scorecard::AnnotationDriftScorecard, String> {
    RATE_LIMITER.check("compute_annotation_drift_scorecard")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        // P1.3: folded from a stream. Only the (small) per-clip error records survive a push, which is
        // all the bootstrap needs; the transcripts they were computed from do not.
        let mut tally = crate::scorecard::AnnotationDriftTally::default();
        db.for_each_segment(None, |seg| tally.push(&seg)).map_err(|e| e.to_string())?;
        Ok(tally.finish(Default::default()))
    })
    .await
}

/// Measured raw-ASR vs post-jury label-quality lift (M3.1) over human-verified segments.
#[tauri::command]
pub async fn get_label_quality_lift(state: State<'_, AppState>) -> Result<crate::eval::LabelQualityLift, String> {
    RATE_LIMITER.check("get_label_quality_lift")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        let triples = crate::eval::load_lift_triples(&db).map_err(|e| e.to_string())?;
        Ok(crate::eval::compute_label_quality_lift(&triples, 2000, 1234))
    })
    .await
}
