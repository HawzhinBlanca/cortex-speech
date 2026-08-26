from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def command_surface() -> str:
    # Week-4 decomposes commands.rs into slices under src/commands/. The agentic command patterns
    # asserted below (agent stage events, consensus refinery, engine status/readiness) may live in a
    # slice, so scan the whole command surface — else the check passes vacuously when a command moves.
    parts = [read("src-tauri/src/commands.rs")]
    commands_dir = ROOT / "src-tauri" / "src" / "commands"
    if commands_dir.is_dir():
        parts += [p.read_text(encoding="utf-8") for p in sorted(commands_dir.rglob("*.rs"))]
    text = "\n".join(parts)
    if "#[tauri::command]" not in text:
        raise AssertionError("no #[tauri::command] in the command surface — this agentic gate would pass vacuously")
    return text


def main() -> None:
    agentic = read("src-tauri/src/agentic.rs")
    commands = command_surface()
    db = read("src-tauri/src/db.rs")
    learning = read("src-tauri/src/jury/learning.rs")
    integration = read("src-tauri/src/integration_runner.rs")
    runs = read("src-tauri/src/runs.rs")
    # pipeline.rs's test module was split into pipeline_tests.rs (#[path]); scan both so a moved
    # regression-test assertion still resolves (else this gate reads it as "missing").
    pipeline = read("src-tauri/src/pipeline.rs")
    _pt = ROOT / "src-tauri" / "src" / "pipeline_tests.rs"
    if _pt.is_file():
        pipeline += "\n" + _pt.read_text(encoding="utf-8")
    # export_bundle.rs's test module was split into export_bundle_tests.rs (#[path]); scan both so a
    # moved regression-test name/helper still resolves (else this gate reads it as "missing").
    export_bundle = read("src-tauri/src/export_bundle.rs")
    _ebt = ROOT / "src-tauri" / "src" / "export_bundle_tests.rs"
    if _ebt.is_file():
        export_bundle += "\n" + _ebt.read_text(encoding="utf-8")
    settings = read("src-tauri/src/settings.rs")
    migrations = read("src-tauri/src/migrations/mod.rs")
    lib_rs = read("src-tauri/src/lib.rs")
    commands_ts = read("src/lib/commands.ts")
    app = read("src/Workstation.svelte")
    status_bar = read("src/lib/StatusBar.svelte")
    agent_report_panel = read("src/lib/AgentReportPanel.svelte")
    settings_store = read("src/lib/stores/settingsStore.ts")
    events = read("src/lib/events.ts")
    ui_store = read("src/lib/stores/uiStore.ts")

    required_agentic = [
        "generate_whole_file_reference_transcript",
        "transcribe_uploaded_audio",
        "INLINE_AUDIO_LIMIT_BYTES",
        "segment_audio_as_wav_base64",
        "chunking::slice_pcm_by_alignment",
        "whole_file_reference.txt",
        "is_usable_source_reference_transcript",
        "select_best_candidate_against_reference",
        "reference_window_overlap",
        "should_commit",
    ]
    for needle in required_agentic:
        assert needle in agentic, f"missing agentic pipeline primitive: {needle}"

    assert "source_transcripts" in migrations, "source_transcripts migration is required"
    assert "agent_import_reports" in migrations, "agent import reports migration is required"
    assert "agent_stage_events" in migrations, "agent stage event persistence migration is required"
    assert "source_reference_models" in settings, "settings must expose source reference model list"
    settings_production = settings.split("#[cfg(test)]", 1)[0]
    assert 'pub const ADVISORY_CLOUD_MODEL: &str = "gemini-2.5-pro";' in settings_production, (
        "the one owner-approved cloud advisor must remain Gemini 2.5 Pro"
    )
    assert "fn default_source_reference_models() -> Vec<String>" in settings_production
    assert "vec![ADVISORY_CLOUD_MODEL.to_string()]" in settings_production, (
        "source-reference defaults must be the canonical singleton, not a second/fast model"
    )
    assert '"gemini-2.5-flash"' not in settings_production, (
        "Gemini Flash must not re-enter production defaults or runtime policy"
    )
    assert "sourceReferenceModels" in settings_store, "frontend settings must expose source reference model list"
    assert "get_latest_source_transcript_for_audio" in db, "jury must be able to reuse latest source reference"
    assert "get_source_transcripts_for_audio" in db, "jury must score against all stored source references"
    assert "audio_content_hash" in db and "audio_size_bytes" in db, (
        "source-reference records must carry audio identity fields so path reuse cannot trust stale references"
    )
    assert "audio_content_hash TEXT" in migrations and "audio_size_bytes INTEGER" in migrations, (
        "source-reference audio identity fields need a schema migration"
    )
    assert "ensure_source_reference_transcripts(path, db)" in pipeline, "import must build/reuse source references"
    assert "reusable_source_reference_record" in pipeline, "import must validate cached source-reference text files"
    assert "source_audio_identity(path)" in pipeline, "source-reference reuse must inspect the current audio bytes"
    assert "existing.audio_content_hash.as_deref()" in pipeline, (
        "cached source-reference reuse must compare stored and current audio hashes"
    )
    assert "std::fs::read_to_string(transcript_path)" in pipeline, (
        "cached source-reference reuse must inspect the saved text file before trusting DB text"
    )
    assert "edited_source_reference_file_syncs_database_before_reuse" in pipeline, (
        "manual source-reference text-file edits need a Rust regression"
    )
    assert "changed_audio_path_does_not_reuse_stale_source_reference" in pipeline, (
        "same-path changed audio must not reuse a stale whole-file reference transcript"
    )
    assert "empty_source_reference_file_is_not_reused" in pipeline, (
        "empty source-reference text files must not be silently reused"
    )
    assert "self.settings.source_reference_models()" in pipeline, "import must use the configured source model list"
    assert "Whole-file reference transcript failed before chunking" in pipeline, (
        "configured source-reference failure must stop import before chunking"
    )
    assert "Gemini API key is required for whole-file reference transcript" in pipeline, (
        "cloud source-reference mode must fail closed when the API key is missing"
    )
    assert "source_reference_cloud_opt_in_without_key_is_fatal_before_chunking" in pipeline, (
        "missing source-reference API key needs a Rust regression"
    )
    assert "configured_source_reference_failure_is_fatal_before_chunking" in pipeline, (
        "source-reference fail-closed behavior needs a Rust regression"
    )
    assert "stale_source_reference_failure_is_fatal_before_chunking" in pipeline, (
        "stale canonical source-reference evidence must fail before chunking"
    )
    assert "refusing to continue with incomplete source-reference evidence" in pipeline, (
        "partial source-reference failures must be explicit before chunking"
    )
    assert "Skipping whole-file reference transcript" not in pipeline, (
        "enabled source-reference mode must not silently fall back when the API key is missing"
    )
    assert "Whole-file reference transcript skipped" not in pipeline, (
        "configured source-reference failures must not be logged and ignored"
    )
    assert "run_primary_wsl_pass_for_import" in pipeline, "WSL 7B imports must attempt the primary ASR pass before jury"
    champion_import_call = "self.run_primary_wsl_pass_for_import(&mut prepared, cancel)?"
    assert pipeline.count(champion_import_call) == 2, (
        "streaming and non-streaming imports must finish the WSL 7B primary pass in memory before publication"
    )
    champion_publish_call = (
        "import_writes.publish_champion_segments(&prepared, deployment_sha256, Some(&identity))?"
    )
    assert pipeline.count(champion_publish_call) == 2, (
        "streaming and non-streaming imports must atomically publish only fully champion-drafted segments"
    )
    assert pipeline.index(champion_import_call) < pipeline.index(champion_publish_call), (
        "champion inference must precede the first canonical file publication"
    )
    assert "if !self.should_use_wsl_primary_asr() || segments.is_empty()" in pipeline, (
        "the WSL pass must be inert after an unconfigured champion has already been refused by preflight"
    )
    assert "populate_wsl_hypothesis_if_configured" in pipeline, (
        "explicit non-champion experiments must retain their optional WSL hypothesis helper"
    )
    # Provenance is no longer a hardcoded string: the champion is content-addressed, so the helper
    # binds the hypothesis to the REGISTRY champion identity and REFUSES a reply that does not match
    # it. Strictly stronger than the old literal, which could not tell two deployments apart.
    assert "crate::registry::champion_identity(db, crate::deployment::OMNIASR_7B_FAMILY)" in pipeline, (
        "the explicit optional WSL hypothesis helper must preserve provenance"
    )
    assert "result.model_version_id != expected.model_version_id" in pipeline, (
        "the helper must REFUSE a reply whose producing identity is not the registry champion, or a "
        "rotated-out model's draft would be stored as the champion's"
    )
    assert "self.run_wsl_segment_transcript(&seg.audio_path" in pipeline, (
        # Robust prefix: the transport is now direct (path + offsets), so the call passes the row's
        # audio path rather than a bare segment id. The invariant is unchanged — the pass uses the
        # real external provider, not the exact argument list.
        "the explicit optional WSL hypothesis helper must use the real external provider"
    )
    assert "should_use_wsl_primary_asr" in pipeline, (
        "WSL 7B primary routing must require its configured client"
    )
    # The configured path is now one INPUT to client resolution rather than the whole answer: a client
    # bundled beside the exe also counts, which is what makes an installed build work with no env var.
    # The invariant is unchanged — WSL routing still requires a RESOLVABLE client.
    assert "resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_some()" in pipeline, (
        "configured-client detection is missing from the WSL route"
    )
    assert "wsl7b_primary_unresolved" in pipeline, (
        "an unconfigured champion needs a shared fail-closed guard"
    )
    # Renamed with the bundled-client work: a missing CONFIGURED script is no longer fatal, because a
    # client bundled beside the exe resolves instead — but it still must never fall back to a local
    # engine. Same guarantee, wider resolution. (pipeline_tests.rs:451)
    assert "wsl_without_explicit_script_uses_bundled_champion_and_never_falls_back" in pipeline, (
        "missing WSL script must preserve champion identity and never become a local fallback"
    )
    assert "public_preflight_fails_immediately_when_default_champion_is_unconfigured" in pipeline, (
        "the unconfigured factory champion needs an immediate preflight refusal regression"
    )
    # Renamed with the bundled-client work. Same scenario and same invariant: the pass runs after a
    # refused preflight against a row holding a "pre-existing transcript" and must not mutate it
    # (pipeline_tests.rs:656, segment id "preflight-refused").
    assert "wsl_primary_import_pass_never_silently_skips_the_bundled_champion" in pipeline, (
        "the later WSL pass must not mutate rows after the fail-closed preflight path"
    )
    assert "self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B" in pipeline, (
        "automatic WSL hypothesis pass must avoid duplicating the primary WSL path"
    )
    assert "parse_wsl_segment_result" in pipeline, (
        "WSL 7B stdout parsing must be centralized and regression-tested"
    )
    assert "WSL 7B primary ASR unavailable before publication" in pipeline, (
        "WSL 7B failures must be explicit before canonical publication or jury adjudication"
    )
    # 2026-08-20 external review — the champion pipeline's crash/consistency contract:
    assert "import HALTED at" in pipeline, (
        "directory import must HALT on the first real failure (owner rule 2026-08-11), never tally and continue"
    )
    assert "halted before this file's {segment_count} segment(s) were published" in pipeline, (
        "an exhausted-empty champion result must halt before publication, never store unresolved rows in a 'successful' import"
    )
    assert "resume_segment_has_authoritative_transcript" in pipeline, (
        "resume must prove complete human/current-champion transcript authority from the rows; rows-exist is not done"
    )
    assert "resume_should_skip_file(resume_completed.is_some(), has_authoritative_segments)" in pipeline, (
        "the resume journal must remain a hint and may skip only rows that independently prove transcript authority"
    )
    assert "resume_authority_rejects_placeholder_wrong_model_cloud_and_blank_drafts" in pipeline, (
        "placeholder, wrong-model, cloud, and blank drafts need an adversarial resume-authority regression"
    )
    assert pipeline.count("committed_by_pipeline: commit_champion") == 1, (
        "the champion result must report the exact conditional commit owner"
    )
    compact_pipeline = " ".join(pipeline.split())
    bound_commit_call = (
        "self.transcribe_with_champion_commit( Some(&source.snapshot.segment.id), "
        "&source.snapshot.segment.audio_path, source.snapshot.segment.alignment_json.as_deref(), "
        "cancel, true, Some(&source.snapshot), )"
    )
    assert bound_commit_call in compact_pipeline, (
        "normal re-transcription must commit only through the immutable id/path/span/content source snapshot"
    )
    import_draft_call = (
        "self.transcribe_with_champion_commit( Some(segment_id), audio_path, alignment_json, cancel, false, None, )"
    )
    assert import_draft_call in compact_pipeline, (
        "import drafting must explicitly disable per-segment publication and carry no false source authority"
    )
    assert "if commit_champion && expected_source.is_none()" in pipeline, (
        "the shared transcription primitive must reject every commit that lacks immutable source authority"
    )
    assert "E_TRANSCRIPTION_SOURCE_UNBOUND" in pipeline, (
        "an unbound transcription commit needs a stable fail-closed error"
    )
    assert ".commit_bound_champion_transcript_if_unreviewed(" in pipeline, (
        "the immediate champion writer must revalidate the bound source snapshot inside its transaction"
    )
    assert "expected_source.ok_or_else" in pipeline, (
        "the final commit call must not synthesize or omit source authority"
    )
    assert "pipeline.bind_existing_transcription_source_cached(" in commands, (
        "batch transcription must bind each segment to the current database/audio snapshot before inference"
    )
    assert "pipeline.transcribe_bound(&source, Some(cancel.as_atomic()))" in commands, (
        "batch transcription must pass only the bound source into the committing pipeline path"
    )
    assert ".bind_existing_transcription_source(id, Some(&audio_path), alignment_json.as_deref())" in commands, (
        "single transcription must refuse caller id/path substitution by binding server source truth"
    )
    assert "pipeline.transcribe_bound(&source, None)" in commands, (
        "single transcription must use the same bound commit path as batch transcription"
    )
    assert "Ok(draft) if draft.committed_by_pipeline =>" in commands, (
        "batch_transcribe must not re-write a draft the pipeline already committed (one inference, one commit, one owner)"
    )
    assert "run_wsl_segment_transcript_direct" in pipeline, (
        "the champion transport is the direct Rust->server protocol, not a per-segment WSL subprocess with a DB copy"
    )
    assert "hypothesis_coverage_guard" in commands, (
        "jury must fail closed when fewer than two chunk-level model hypotheses are available"
    )
    assert "quality::hypothesis_coverage_for_model_outputs" in commands, (
        "jury hypothesis coverage guard must share the export/training coverage calculation"
    )
    assert "fewer than 2 non-empty model hypotheses" in commands, (
        "incomplete hypothesis coverage must be explicit in the jury rationale"
    )
    assert "hypothesisGuarded" in commands, "jury summary must report hypothesis-coverage guarded segments"
    assert "incomplete_hypothesis_model_coverage_blocks_t0_and_t1_auto_commit" in commands, (
        "incomplete chunk-level model coverage needs a Rust regression"
    )
    for needle in [
        "pub struct AgenticReadiness",
        "pub struct AgenticReadinessCheck",
        "build_agentic_readiness",
        "check_agentic_readiness",
        "source_reference",
        "primary_asr",
        "hypothesis_coverage",
        "available_hypothesis_models",
        "required_hypothesis_models",
        "agentic_readiness_offline_source_reference_is_not_required_not_blocked",
        "disabled_cloud_reports_not_required_not_ready_but_keeps_the_overall_verdict_ready",
        "champion_readiness_ignores_installed_optional_ctc_models",
        "local_readiness_checks_the_exact_explicitly_selected_engine",
        "settings.multi_engine_hypotheses && settings.asr_model_size != AsrModelSize::WSL7B",
        "The selected primary engine is unavailable; refusing to substitute another engine.",
        "Single-engine mode: {primary_model_id} is the only automatic ASR source. Optional engines will not run.",
    ]:
        assert needle in commands, f"agentic readiness preflight must cover {needle}"
    assert "commands::check_agentic_readiness" in lib_rs, "Tauri handler must register agentic readiness preflight"
    assert "AgenticReadiness" in commands_ts, "frontend command API must type agentic readiness"
    assert "checkAgenticReadiness" in commands_ts, "frontend command API must expose agentic readiness"
    assert "agenticReadiness?: AgenticReadiness | null" in commands_ts, (
        "frontend agent report summary must type the persisted readiness snapshot"
    )
    assert "warnAgenticReadinessBeforeImport" in app, "file and directory imports must run the agentic readiness preflight"
    assert "api.checkAgenticReadiness()" in app, "import preflight must call the backend readiness command"
    assert "agenticReadiness.blocked" in app, "blocked readiness must be visible before import"
    assert "agenticReadiness.degraded" in app, "degraded readiness must be visible before import"
    assert "Post-import jury adjudication failed after directory import" in pipeline, (
        "directory import must fail visibly when post-import jury adjudication fails"
    )
    assert "record_agent_import_report" in pipeline, (
        "directory import must persist an auditable agent import report"
    )
    assert "Agent import report persistence failed after directory import" in pipeline, (
        "directory import must fail visibly if the audit report cannot be persisted"
    )
    assert "return Err(AppError::Other(message));" in pipeline, (
        "directory import must not report success after post-import jury adjudication fails"
    )
    assert "Jury pipeline failed after directory import: {error}" not in pipeline, (
        "directory import jury failures must not be log-only"
    )
    assert '"reference_transcribing"' in pipeline, "pipeline must emit reference transcript phase"
    assert '"adjudicating"' in pipeline, "pipeline must emit adjudication phase before jury"
    assert "PipelineEvent::AgentStage" in pipeline, "pipeline must expose live agent stage events"
    assert '"source_reference"' in pipeline, "live timeline must include source-reference stage"
    assert '"audio_chunking"' in pipeline, "live timeline must include chunking stage"
    assert '"multi_model_hypotheses"' in pipeline, "live timeline must include multi-model hypothesis stage"
    assert '"jury_adjudication"' in pipeline, "live timeline must include jury stage"
    assert '"agent_report"' in pipeline, "live timeline must include report persistence stage"
    assert "Post-import jury adjudication failed after single-file import" in commands, (
        "single-file import must fail visibly when post-import jury adjudication fails"
    )
    assert "Agent import report persistence failed after single-file import" in commands, (
        "single-file import must fail visibly if the audit report cannot be persisted"
    )
    assert "record_agent_import_report" in commands, (
        "single-file import must persist an auditable agent import report"
    )
    assert "record_agent_stage_event" in commands, "single-file import must persist agent stage events"
    assert "report_options.agent_run_id = Some(agent_run_id.clone())" in commands, (
        "single-file import reports must store the same run id used for stage events"
    )
    assert "list_agent_stage_events" in commands, "backend must expose persisted agent stage events"
    assert "App state unavailable for post-import jury adjudication" in commands, (
        "single-file import must fail closed if it cannot access app state for jury"
    )
    assert "list_agent_import_reports" in commands, "backend must expose agent import report listing"
    assert "commands::list_agent_import_reports" in lib_rs, "Tauri handler must register report listing command"
    assert "commands::list_agent_stage_events" in lib_rs, "Tauri handler must register stage event listing command"
    assert "listAgentImportReports" in commands_ts, "frontend command wrapper must expose report listing"
    assert "listAgentStageEvents" in commands_ts, "frontend command wrapper must expose stage event listing"
    assert "AgentImportReport" in commands_ts, "frontend must type agent import reports"
    assert "AgentStageEvent" in commands_ts, "frontend must type persisted agent stage events"
    assert "agentRunId" in commands_ts, "frontend must type the report-to-stage run id"
    assert "AgentReportPanel" in app, "app must render the latest agent import report"
    assert "agentPipelineStages" in status_bar, "status bar must render live agent pipeline stages"
    assert "data-testid=\"agent-pipeline-timeline\"" in status_bar, "live agent timeline needs a stable UI hook"
    assert "loadLatestAgentReport" in app, "app must refresh the latest agent import report"
    assert "loadLatestAgentStageEvents" in app, "app must refresh persisted agent stage events"
    assert "latestAgentReport?.agentRunId" in app, "app must load the stage log for the latest report run id"
    assert "await loadLatestAgentReport()" in app, "import completion must refresh the latest agent report"
    assert "await loadLatestAgentStageEvents()" in app, "import completion must refresh the persisted stage log"
    assert "stageEvents={latestAgentStageEvents}" in app, "agent report panel must receive persisted stage events"
    assert "dataset_promotion" in app, "HF export UI must inspect the dataset promotion stage"
    assert "trainingExportBlocked" in app, "HF export UI must block when promotion readiness is blocked"
    assert "exportHuggingface.blocked" in app, "HF export block reason must be user-visible"
    assert "data-testid=\"agent-report-panel\"" in agent_report_panel, (
        "agent report panel needs a stable UI regression hook"
    )
    for needle in [
        "sourceReferenceModels",
        "requiredSourceReferenceModels",
        "sourceReferenceCoverage",
        "longFileDossiers",
        "hypothesisModels",
        "hypothesisModelCounts",
        "trainingGradeSummary",
        "trainingGradeReasonCounts",
        "hypothesisCoverageBlockers",
        "agenticReadiness",
        "readyHypothesisModels",
        "orchestrationStages",
        "verdictCounts",
        "escalatedSegments",
        "transcriptPath",
        "audioContentHash",
        "audioSizeBytes",
        "agent-report-source-files",
        "agent-report-source-reference-coverage",
        "agent-report-long-file-dossiers",
        "agent-report-escalated-ids",
        "agent-report-model-coverage",
        "agent-report-agentic-readiness",
        "agent-report-orchestration-stages",
        "agent-report-persisted-stage-events",
        "agent-report-coverage-blockers",
        "agent-report-grade-reasons",
    ]:
        assert needle in agent_report_panel, f"agent report panel must surface {needle}"
    required_runs = [
        "pub struct AgentStageEvent",
        "record_agent_stage_event",
        "list_agent_stage_events",
        "pub struct AgentImportReport",
        "pub struct AgentImportSummary",
        "pub agent_run_id: Option<String>",
        "agentic_readiness",
        "record_agent_import_report",
        "list_agent_import_reports",
        "get_source_transcripts_for_audio",
        "get_hypotheses_for_segment",
        "hypothesis_model_counts",
        "source_reference_required",
        "required_source_reference_models",
        "source_reference_coverage",
        "audio_content_hash",
        "audio_size_bytes",
        "long_file_dossiers",
        "AgentLongFileDossier",
        "build_long_file_dossiers",
        "AgentSourceReferenceCoverage",
        "pub struct AgentImportReportOptions",
        "pub fn from_settings",
        "training_grade_reason_counts",
        "hypothesis_coverage_blockers",
        "orchestration_stages",
        "build_orchestration_stages",
        "AgentOrchestrationStage",
        "summary.agentic_readiness",
        "records_and_filters_agent_stage_events_by_run",
        "records_agent_import_report_source_reference_model_coverage_blockers",
        "records_agent_import_report_hypothesis_coverage_blockers",
        "training_grade_summary",
        "records_agent_import_report_with_multi_agent_evidence_summary",
    ]
    for needle in required_runs:
        assert needle in runs, f"missing agent import report primitive: {needle}"
    assert "record_agent_import_report_with_options" in pipeline, (
        "directory import reports must include configured source-reference coverage expectations"
    )
    assert "import_directory_with_agent_run_id" in pipeline, (
        "directory import reports must be able to store the stage-event run id"
    )
    assert "report_options.agent_run_id = agent_run_id.map(str::to_string)" in pipeline, (
        "directory import reports must store the same run id used for stage events"
    )
    assert "report_options.agentic_readiness" in pipeline, (
        "directory import reports must preserve the runtime agentic readiness snapshot"
    )
    for needle in [
        "fn multi_model_hypothesis_stage",
        "get_hypotheses_for_segment",
        "hypothesis_coverage_for_model_outputs",
        "coverage.passes_minimum",
        "Verified multi-model hypothesis coverage",
        "Only {covered}/{total} segment(s) have the required non-empty multi-model hypothesis coverage",
        "multi_model_hypothesis_stage_reports_verified_coverage",
        "multi_model_hypothesis_stage_blocks_incomplete_coverage",
    ]:
        assert needle in pipeline, f"pipeline stage timeline must be backed by stored hypothesis coverage: {needle}"
    assert "Chunk-level model hypotheses populated for {segment_count} segment(s)" not in pipeline, (
        "multi-model hypothesis stage must not claim completion without checking stored coverage"
    )
    assert "record_agent_import_report_with_options" in commands, (
        "single-file import reports must include configured source-reference coverage expectations"
    )
    assert "build_agentic_readiness_snapshot" in commands, (
        "agentic readiness must be serializable into persisted import reports"
    )
    assert "report_options.agentic_readiness" in commands, (
        "single-file import reports must preserve the runtime agentic readiness snapshot"
    )
    for needle in [
        "training_grade_details.json",
        "build_training_grade_details",
        "sourceReference",
        "hypothesisModelCounts",
        "hypothesisCoverage",
        "source_reference_manifest.json",
        "write_source_reference_artifacts",
        "collect_source_reference_bundle_records",
        "source_reference_export_identity_blockers",
        "audioIdentityVerified",
        "audioIdentityIssue",
        "Production export blocked: source-reference transcripts have missing or stale audio identity",
        "production_export_blocks_source_reference_without_audio_identity",
        "production_export_blocks_stale_source_reference_audio_identity",
        "source_transcripts",
        "audioContentHash",
        "audioSizeBytes",
        "whole_file_reference.txt",
        "learning_manifest.json",
        "learning_preferences.jsonl",
        "write_learning_artifacts",
        "build_dpo_dataset_for_segment_ids(db, &selected_segment_ids)",
        "draft_export_includes_self_learning_preference_artifacts",
        "agent_provenance.json",
        "long_file_dossiers.json",
        "longFileDossierCount",
        "agentImportReports",
        "agenticReadiness",
        "agentImportReportCount",
        "agentRunId",
        "agentStageEvents",
        "agentStageEventCount",
        "agent_stage_event_limit = 500usize",
        "list_stage_events_for_reports(db, &agent_import_reports, agent_stage_event_limit)",
        "list_agent_stage_events(db, Some(run_id.as_str()), Some(remaining))",
        "agentPromotionReadiness",
        "sourceReferenceCount",
        "agent_import_report_limit = 200usize",
        "list_agent_import_reports(db, Some(agent_import_report_limit))",
        "draft_export_preserves_agent_import_provenance",
        "bundle_path_label",
        "sanitize_bundle_public_fields",
        "sensitive_reviewer_identity_key",
        "sanitized_bundle_value",
        '"audioPath" | "transcriptPath" | "originalTranscriptPath" | "file"',
        '"audioPaths"',
        "assert_json_strings_do_not_contain",
    ]:
        assert needle in export_bundle, f"bundle export must preserve agent provenance: {needle}"
    assert "if let Err(error) = run_jury_pipeline_core(&db, &settings, segment_ids.clone())" not in commands, (
        "single-file import jury failures must not be log-only"
    )
    assert "reference_selection_for_segment" in commands, "jury pipeline must score candidates against source reference"
    assert "get_source_transcripts_for_audio" in commands, "jury must use all source references, not only the latest"
    assert "source_reference_coverage_guard" in commands, (
        "jury must fail closed when configured source-reference model coverage is incomplete"
    )
    assert "source_reference_matches_current_audio" in commands, (
        "jury must re-check stored source-reference audio identity before using cached whole-file transcripts"
    )
    assert "Source-reference audio identity guard blocked automatic adjudication" in commands, (
        "stale source references must have an explicit jury guard rationale"
    )
    assert "missing audio identity or no longer match the current source audio bytes" in commands, (
        "source-reference identity guard must fail closed for both missing and mismatched audio identity"
    )
    assert "stale_source_reference_audio_identity_blocks_automatic_jury_commit" in commands, (
        "same-path changed audio needs a jury-time stale source-reference regression"
    )
    assert "missing_source_reference_audio_identity_blocks_automatic_jury_commit" in commands, (
        "legacy source references without audio identity need a jury-time regression"
    )
    assert "settings.source_reference_models()" in commands, (
        "jury coverage guard must use configured source-reference models"
    )
    assert "configured whole-file reference models are missing or empty" in commands, (
        "incomplete source-reference coverage must be explicit in the jury rationale"
    )
    assert "incomplete_source_reference_model_coverage_blocks_auto_commit" in commands, (
        "incomplete configured source-reference coverage needs a Rust regression"
    )
    assert "reference_reports_have_commit_consensus" in commands, (
        "multiple whole-file references must agree before source-reference auto-commit"
    )
    assert "referenceAgreement" in commands, "multi-reference decisions must preserve per-reference evidence"
    assert "agreeing_source_references_preserve_per_model_evidence" in commands, (
        "multi-reference consensus evidence needs a regression"
    )
    assert "multi-reference-consensus" in commands, "source-reference evidence must label multi-reference consensus"
    assert "source_reference_disagreement_blocks_automatic_commit" in commands, (
        "conflicting whole-file references need a regression"
    )
    assert "referenceCommitted" in commands, "jury summary must report reference-agent commits"
    assert "referenceGuarded" in commands, "jury summary must report source-reference guarded segments"
    assert "referenceSelection" in commands, "jury evidence must preserve reference scoring details"
    assert "Source-reference adjudication runs before T0" in commands, (
        "source-reference adjudication must run before T0 auto-accept"
    )
    assert "let t0_report = if t0_candidate_ids.is_empty()" in commands, (
        "T0 must run only after source-reference preflight chooses T0 candidates"
    )
    assert "let effective_t1_threshold = if reference_report.is_some() { 1.01 } else { t1_threshold };" in commands, (
        "T1 text-only commits must be blocked when source-reference evidence is inconclusive"
    )
    assert "crate::agentic::segment_audio_as_wav_base64(&seg)" in commands, (
        "T2 jury must receive the segment span, not the whole source file"
    )
    assert '"t2Transcript": verdict.transcript.clone()' in commands, (
        "T2 jury commits must persist the selected transcript so training promotion can verify evidence-text parity"
    )
    assert '"t2Evidence": verdict.evidence.clone()' in commands, (
        "T2 jury commits must keep the underlying listener evidence for auditability"
    )
    assert "std::fs::read(&seg.audio_path)" not in commands, (
        "T2 jury must not base64 encode the whole source file"
    )
    assert "'reference_transcribing'" in ui_store, "frontend phase type must include reference_transcribing"
    assert "'adjudicating'" in ui_store, "frontend phase type must include adjudicating"
    assert "agentPipelineStages" in ui_store, "frontend must keep a live agent timeline store"
    assert "upsertAgentPipelineStage" in ui_store, "frontend must merge live agent stage updates by stage"
    assert "reference_transcribing" in events, "frontend event listener must accept reference_transcribing"
    assert "adjudicating" in events, "frontend event listener must accept adjudicating"
    assert "pipeline-agent-stage" in commands, "backend must emit live agent stage events through Tauri"
    assert "pipeline-agent-stage" in events, "frontend must listen for live agent stage events"
    assert "AgentPipelineStageEvent" in events, "frontend must type live agent stage payloads"
    assert "build_learning_prompt" in learning, "self-learning export must build multi-agent prompt context"
    assert "get_hypotheses_for_segment" in learning, "DPO export must include model hypotheses"
    assert "get_source_transcripts_for_audio" in learning, "DPO export must include whole-file source references"
    assert "learning_audio_path_label" in learning, "DPO prompts must sanitize local audio paths before export"
    assert ".file_name()" in learning, "DPO prompt audio context should use basename-only labels"
    assert "current_audio_source_references" in learning, (
        "DPO source-reference context must be filtered against the current audio identity"
    )
    assert "source_reference_matches_current_audio" in learning, (
        "DPO learning must not trust source references by path alone"
    )
    assert "source_audio_identity(Path::new(audio_path))" in learning, (
        "DPO source-reference filtering must hash the current audio bytes"
    )
    assert "learning_evidence_has_current_source_reference_coverage" in learning, (
        "DPO evidence JSON must not leak stale source-reference decisions into learning prompts"
    )
    assert "source_reference_model_ids_in_evidence" in learning, (
        "DPO evidence filtering must inspect referenced source-reference model ids"
    )
    assert "append_reference_agreement_summary" in learning, (
        "DPO export must summarize multi-reference agreement votes"
    )
    assert '"Reference agreement:"' in learning, "DPO prompt must expose structured reference-agreement context"
    assert "build_dpo_dataset_summarizes_reference_agreement_votes" in learning, (
        "reference-agreement learning context needs a Rust regression"
    )
    assert "build_dpo_dataset_ignores_stale_source_reference_context" in learning, (
        "stale source-reference learning context needs a Rust regression"
    )
    assert "build_dpo_dataset_sanitizes_prompt_audio_path" in learning, (
        "DPO prompt path sanitization needs a Rust regression"
    )
    assert '!pair.prompt.contains("jury_evidence_json")' in learning, (
        "stale source-reference evidence JSON must be absent from DPO prompts"
    )
    assert "learning_text_key(wrong) == learning_text_key(fix)" in learning, (
        "DPO export must skip no-op duplicate correction pairs"
    )
    assert "rejected_transcript_for_learning" in db, "human edits must learn from the actual rejected agent proposal"
    assert "crate::commands::run_jury_pipeline_core(&db, &settings, segment_ids.clone())" in integration, (
        "headless audiobook pipeline must adjudicate imported chunks before export"
    )
    for needle in [
        "let agent_run_id = uuid::Uuid::new_v4().to_string()",
        "record_agent_import_report_with_options",
        "AgentImportReportOptions::from_settings",
        "options.agent_run_id = Some(agent_run_id.to_string())",
        "audiobook_agentic_readiness_snapshot",
        "options.agentic_readiness",
        "record_agent_stage_event",
        '"audiobook"',
        "crate::export_bundle::export_dataset_bundle",
        'bundle={}',
    ]:
        assert needle in integration, f"headless audiobook pipeline must preserve agent provenance: {needle}"
    jury_pos = integration.find("crate::commands::run_jury_pipeline_core(&db, &settings, segment_ids.clone())")
    export_pos = integration.find("export::export_dataset(&db, &json_path")
    if jury_pos < 0 or export_pos < 0 or jury_pos > export_pos:
        raise AssertionError("headless audiobook jury must run before JSON/JSONL/Parquet export")

    print("agentic pipeline policy regression passed")


if __name__ == "__main__":
    main()
