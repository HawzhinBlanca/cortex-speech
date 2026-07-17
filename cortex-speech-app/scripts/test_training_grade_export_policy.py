from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def export_surface() -> str:
    # export.rs's #[cfg(test)] module was split into export_tests.rs via #[path] (Week-4 decomposition).
    # Several assertions below pin #[test] fn NAMES (the HuggingFace skip/write regressions) that now
    # live in export_tests.rs — so the gate must scan the whole export SURFACE. Reading only export.rs
    # would let a moved regression vanish and this policy pass vacuously.
    parts = [read("src-tauri/src/export.rs")]
    tests = ROOT / "src-tauri/src/export_tests.rs"
    if tests.is_file():
        parts.append(tests.read_text(encoding="utf-8"))
    text = "\n".join(parts)
    if "#[test]" not in text or "ExportSegmentRecord::new" not in text:
        raise AssertionError(
            "export surface is missing production or test code — this training-grade gate would pass vacuously"
        )
    return text


def main() -> None:
    quality = read("src-tauri/src/quality.rs")
    export = export_surface()
    export_bundle = read("src-tauri/src/export_bundle.rs")

    required_quality = [
        "TrainingGradeReport",
        "TrainingGradeSummary",
        "TRAINING_GRADE_GOLD",
        "TRAINING_GRADE_SILVER",
        "TRAINING_GRADE_REVIEW",
        "TRAINING_GRADE_REJECT",
        "training_grade_for_segment",
        "training_grade_summary",
        "human_rejected",
        "placeholder_transcript",
        "is_placeholder_transcript(effective_transcript(seg))",
        '"[Pending WSL 7B ASR]"',
        "severe_clipping",
        "high_confidence_jury_accept",
        "multi_agent_evidence_verified",
        "missing_multi_agent_evidence",
        "has_training_ready_machine_evidence",
        "has_source_reference_commit_evidence",
        "has_source_reference_commit_evidence_for_transcript",
        "has_t2_listener_evidence",
        "has_t2_listener_evidence_for_transcript",
        "evidence_transcript_matches",
        '"selectedTranscript": "jury selected transcript"',
        '"t2Transcript": "t2 selected transcript"',
        "HypothesisCoverageReport",
        "MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE",
        "hypothesis_coverage_for_model_outputs",
        "training_grade_ignores_individual_reference_votes_without_consensus_commit",
        "training_grade_keeps_mismatched_reference_evidence_in_review",
        "training_grade_keeps_mismatched_t2_evidence_in_review",
    ]
    for needle in required_quality:
        assert needle in quality, f"missing training grade policy primitive: {needle}"

    required_export_fields = [
        "training_transcript",
        "transcript_source",
        "training_grade",
        "training_ready",
        "training_reasons",
        'Field::new("training_ready"',
        "ExportSegmentRecord::new",
        "quality::training_grade_for_segment",
    ]
    for needle in required_export_fields:
        assert needle in export, f"missing training grade export field: {needle}"

    assert "if !grade.training_ready" in export, "HF export must skip review/reject rows"
    assert "is_training_ready_for_huggingface_export" in export, (
        "HF export must apply DB-backed model coverage checks before writing training rows"
    )
    assert "quality::hypothesis_coverage_for_model_outputs" in export, (
        "HF export must use the shared hypothesis coverage rule"
    )
    assert "export_huggingface_skips_machine_ready_rows_without_hypothesis_coverage" in export, (
        "HF export needs a regression for weak machine-ready rows"
    )
    assert "ready_agentic_huggingface_segment_ids" in export, (
        "HF export must derive the latest ready agentic report coverage before writing machine rows"
    )
    assert "ready_agentic_segment_ids.contains(&segment.id)" in export, (
        "HF export must require machine-ready rows to be covered by the latest ready agentic report"
    )
    assert "source_reference_identity_verified_for_huggingface_export" in export, (
        "HF export must verify current source-reference audio identity before writing machine rows"
    )
    assert "let required_source_reference_models = settings.source_reference_models()" in export, (
        "HF export must derive configured source-reference model coverage from settings"
    )
    assert "required_source_reference_models: &[String]" in export, (
        "HF export source-reference identity gate must receive the configured required model list"
    )
    assert "segment_has_source_reference_commit_evidence" in export, (
        "HF export must apply the source-reference identity gate only to source-reference-driven machine rows"
    )
    assert "quality::has_source_reference_commit_evidence" in export, (
        "HF export must share the training-grade source-reference evidence parser"
    )
    assert "source_reference_record_matches_current_audio" in export, (
        "HF export must compare stored source-reference identity to current audio bytes"
    )
    assert "machine training-ready row is missing multi-model hypothesis coverage, ready agentic promotion coverage, or configured source-reference model coverage/current audio identity" in export, (
        "HF export skip reason must cover missing configured source-reference coverage and current identity"
    )
    assert "export_huggingface_skips_machine_ready_rows_without_ready_agentic_report" in export, (
        "HF export needs a regression for missing ready agentic report"
    )
    assert "export_huggingface_skips_machine_ready_rows_not_covered_by_latest_ready_agentic_report" in export, (
        "HF export needs a regression for stale agentic report coverage"
    )
    assert "export_huggingface_skips_machine_ready_rows_without_current_source_reference_identity" in export, (
        "HF export needs a regression for legacy source-reference records without audio identity"
    )
    assert "export_huggingface_skips_machine_ready_rows_missing_configured_source_reference_model" in export, (
        "HF export needs a regression for incomplete configured source-reference model coverage"
    )
    assert "export_huggingface_skips_machine_ready_rows_with_stale_source_reference_identity" in export, (
        "HF export needs a regression for stale source-reference audio identity"
    )
    assert "export_huggingface_writes_machine_ready_rows_with_matching_ready_agentic_report" in export, (
        "HF export needs a positive regression for covered machine-ready rows"
    )
    assert "canonical_training_text(&grade.transcript)" in export, (
        "exports must write the CANONICALIZED rubric-selected training transcript (grade.transcript), "
        "so JSON/JSONL/CSV/Parquet/HF all emit byte-identical training text and no mixed orthography "
        "re-enters the corpus"
    )
    assert "training_grade_summary.json" in export_bundle, "bundle must include grade summary artifact"
    assert "training_grade_details.json" in export_bundle, "bundle must include per-segment grade details"
    assert "build_training_grade_details" in export_bundle, "bundle must build auditable per-segment grade details"
    assert "hypothesisModelCounts" in export_bundle, "grade details must expose per-segment model coverage"
    assert "hypothesisCoverage" in export_bundle, "grade details must expose model coverage pass/fail evidence"
    assert "minimumNonEmptyModelCount" in export_bundle, "model coverage evidence must expose the required minimum"
    assert "passesMinimum" in export_bundle, "model coverage evidence must say whether the segment passes"
    assert "training_grade_details_records_hypothesis_coverage_evidence" in export_bundle, (
        "hypothesis coverage evidence needs a Rust regression"
    )
    assert "sourceReference" in export_bundle, "grade details must expose source-reference evidence"
    assert "trainingReadySegmentCount" in export_bundle, "grade details must expose training-ready segment count"
    assert "trainingReadySegments" in export_bundle, "bundle manifest must expose training-ready count"
    assert "agentPromotionReadiness" in export_bundle, (
        "bundle manifest and provenance must expose the latest agentic dataset-promotion readiness proof"
    )
    assert "build_agent_promotion_readiness" in export_bundle, (
        "bundle export must derive a compact promotion readiness proof from agent reports"
    )
    assert "training_grade_summary.training_ready_segments == 0" in export_bundle, (
        "production bundle export must block when no segment is training-ready"
    )
    assert "production_export_blocks_when_no_segments_are_training_ready" in export_bundle, (
        "production training-ready gate needs a regression test"
    )
    assert "training_ready_machine_segments_missing_hypothesis_coverage" in export_bundle, (
        "production bundle export must inspect training-ready machine rows for hypothesis coverage"
    )
    assert "Production export blocked: training-ready machine segments are missing multi-model hypothesis coverage" in export_bundle, (
        "production bundle export must block weak machine-ready rows explicitly"
    )
    assert "production_export_blocks_machine_ready_rows_without_hypothesis_coverage" in export_bundle, (
        "machine-ready production coverage gate needs a regression test"
    )
    assert "production_agentic_machine_readiness_blocker" in export_bundle, (
        "production machine-ready bundle export must require a ready agentic promotion proof"
    )
    assert "ready agentic promotion report" in export_bundle, (
        "production agentic promotion blocker must explain missing or blocked readiness"
    )
    assert "production_export_blocks_machine_ready_rows_without_ready_agentic_promotion_report" in export_bundle, (
        "machine-ready production exports need a regression for missing agentic promotion proof"
    )
    assert "production_export_allows_machine_ready_rows_with_ready_agentic_promotion_report" in export_bundle, (
        "ready agentic production machine exports need a positive regression"
    )
    assert "agentic_readiness: report.summary.agentic_readiness.clone()" in export_bundle, (
        "compact promotion readiness proof must carry the persisted agentic readiness snapshot"
    )
    assert "segment_ids: report.segment_ids.clone()" in export_bundle, (
        "compact promotion readiness proof must identify the report-covered segment ids"
    )
    assert "not covered by the latest ready agentic promotion report" in export_bundle, (
        "production machine exports must block stale ready reports that do not cover current machine rows"
    )
    assert "production_export_blocks_machine_ready_rows_not_covered_by_latest_agentic_promotion_report" in export_bundle, (
        "stale agentic promotion proof needs a production export regression"
    )
    assert "production_export_allows_human_gold_without_hypothesis_coverage" in export_bundle, (
        "human-gold production rows must not require machine hypothesis coverage"
    )
    assert "jury_accepted && confidence >= 0.85 && !has_review_risk && has_multi_agent_evidence" in quality, (
        "machine silver rows must require strong multi-agent evidence, not confidence alone"
    )
    assert "training_grade_keeps_high_confidence_jury_without_multi_agent_evidence_in_review" in quality, (
        "high-confidence machine verdicts without strong evidence need a regression"
    )
    assert '.get("referenceSelection")' in quality, (
        "source-reference training evidence must not recurse into conflicting per-reference votes"
    )

    print("training grade export policy regression passed")


if __name__ == "__main__":
    main()
