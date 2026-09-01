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

/// Wave-4 state-boundary coverage: each read command invoked through a genuine managed
/// `State<'_, AppState>`, so the limiter check, the settings snapshot and the blocking closure run
/// exactly as production IPC runs them. The analytics engines are covered in their own modules.
#[cfg(test)]
mod state_command_surface_tests {
    use super::*;
    use crate::test_support::managed_app_state;
    use tauri::Manager;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().expect("build test runtime").block_on(future)
    }

    fn wire<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).expect("serialize analytics payload")
    }

    /// The four dataset readouts on an empty library. An empty corpus must report an honest zero
    /// through the whole wrapper — never a refusal, and never a number nothing measured.
    #[test]
    fn dataset_readouts_report_an_honest_zero_for_an_empty_library() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let stats = wire(&block_on(get_dataset_stats(app.state())).expect("dataset stats"));
        assert_eq!(stats["totalSegments"], 0);
        assert_eq!(stats["verifiedCount"], 0);
        assert_eq!(stats["pendingCount"], 0);
        assert_eq!(stats["verificationRate"], 0.0, "0/0 must not become 100% verified");
        assert_eq!(stats["reviewTiming"]["decisionsLogged"], 0);
        assert_eq!(stats["reviewTiming"]["medianSeconds"], serde_json::Value::Null, "no timing is Null, not 0");
        assert_eq!(stats["topSpeakers"].as_array().map(Vec::len), Some(0));
        assert!(stats["dbSizeBytes"].as_u64().unwrap_or(0) > 0, "the real file-backed library has a real size");

        let quality = wire(&block_on(get_dataset_quality(app.state())).expect("dataset quality"));
        assert_eq!(quality["totalSegments"], 0);
        assert_eq!(quality["emptyTranscriptCount"], 0);
        assert_eq!(quality["duplicateTranscriptGroups"], 0);
        assert_eq!(quality["meanCer"], serde_json::Value::Null, "no reference text means no measured CER");
        assert_eq!(quality["meanWer"], serde_json::Value::Null);
        assert_eq!(quality["qualityGatePassed"], true);

        let grade = wire(&block_on(get_training_grade_breakdown(app.state())).expect("training grade breakdown"));
        assert_eq!(grade["summary"]["totalSegments"], 0);
        assert_eq!(grade["summary"]["trainingReadySegments"], 0);
        assert_eq!(grade["summary"]["goldSegments"], 0);
        assert_eq!(grade["reasonCounts"], serde_json::json!({}), "no rows means no grade reasons to tally");

        let report = wire(&block_on(validate_dataset_cmd(app.state())).expect("dataset validation"));
        assert_eq!(report["totalSegments"], 0);
        assert_eq!(report["passed"], 0);
        assert_eq!(report["summary"], "All 0 segments passed validation checks");
        assert_eq!(report["errors"].as_array().map(Vec::len), Some(0));
        assert_eq!(report["warnings"].as_array().map(Vec::len), Some(0));
    }

    /// The three evidence readouts. Each one's job is to say "nothing measured yet" without
    /// implying a guarantee it does not have.
    #[test]
    fn evidence_readouts_declare_zero_observations_without_implying_a_guarantee() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let intel = wire(&block_on(get_intelligence_report(app.state())).expect("intelligence report"));
        assert_eq!(intel["loop0Shadow"]["totalObservations"], 0);
        assert_eq!(intel["loop0Shadow"]["wouldFire"], 0);
        assert_eq!(intel["autoAcceptPrecision"]["t0Accepts"], 0);
        assert_eq!(intel["autoAcceptPrecision"]["t1Escalations"], 0);
        assert_eq!(intel["conformalCalibration"]["targetErrorCer"], 0.05);
        let buckets = intel["conformalCalibration"]["buckets"].as_array().expect("SNR buckets");
        assert_eq!(buckets.len(), 5, "the calibration readout is bucketed by SNR band");
        assert!(buckets.iter().all(|b| b["verifiedWithReference"] == 0));

        let certificate =
            wire(&block_on(get_dataset_certificate(app.state(), 0.05, 0.95)).expect("dataset certificate"));
        assert_eq!(certificate["targetError"], 0.05, "the requested probabilities round-trip verbatim");
        assert_eq!(certificate["confidenceLevel"], 0.95);
        assert_eq!(certificate["isCalibrated"], false, "zero calibration rows can never be calibrated");
        assert_eq!(certificate["totalCertified"], 0);
        assert_eq!(certificate["certifiedSegmentIds"].as_array().map(Vec::len), Some(0));
        assert_eq!(certificate["calibrationRealPosterior"], 0);
        assert_eq!(certificate["calibrationHeuristic"], 0);
        assert_eq!(certificate["calibrationNoConfidence"], 0);

        let lift = wire(&block_on(get_label_quality_lift(app.state())).expect("label quality lift"));
        assert_eq!(lift["n"], 0);
        assert_eq!(lift["cerLift"], 0.0, "no triples means no measured lift");
        assert_eq!(lift["liftCiLow"], 0.0);
        assert_eq!(lift["liftCiHigh"], 0.0);
    }

    /// The parameter guard runs at the command boundary, AFTER the limiter and BEFORE any DB work —
    /// so a nonsense probability never reaches a full-corpus scan.
    #[test]
    fn the_certificate_command_refuses_non_probability_parameters() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        for (target, confidence, why) in [
            (f64::NAN, 0.95, "a non-finite target error"),
            (f64::INFINITY, 0.95, "an infinite target error"),
            (0.0, 0.95, "a zero target error"),
            (1.5, 0.95, "a target error above 1"),
            (0.05, 0.0, "a zero confidence level"),
            (0.05, 1.0, "a confidence level of exactly 1"),
        ] {
            let refused = match block_on(get_dataset_certificate(app.state(), target, confidence)) {
                Ok(_) => panic!("{why} must be refused, not certified"),
                Err(error) => error,
            };
            assert_eq!(refused.code, "INVALID_CERTIFICATE_PARAMETERS", "{why}");
            assert!(!refused.retryable, "{why} is a caller error, not a transient one");
        }
    }
}
