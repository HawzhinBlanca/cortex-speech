#!/usr/bin/env python3
"""Champion supremacy + hard stop (owner rule, 2026-08-11). See CLAUDE.md.

Both halves exist because BOTH failed silently on the same real run:

  * A 494-clip review queue was drafted 494/494 by `finetuned-mms-ckb` while `asr_model_size` said
    WSL7B and the champion sat up and idle on both GPUs. `use_finetuned_asr` diverted every clip and
    nothing said so. Historical duplication-weighted experiments showed material engine differences,
    but they are not current model evidence; the operational defect is silent substitution itself.
  * 25 clips whose container the champion could not decode failed one at a time, were counted, and the
    batch ran to "completion" — leaving 462 clips at champion quality and 25 at a weaker engine,
    invisibly mixed.

A partly-drafted dataset that LOOKS finished is worse than a run that stopped, because the mixed
provenance silently poisons every measurement taken from it afterwards. These are source pins: the
behaviour lives across a threaded batch loop and a WSL server, which no unit test can exercise here.
"""

from __future__ import annotations

import re
from pathlib import Path

from _pipeline_policy_util import pipeline_surface
from _command_policy_util import command_surface

SRC = Path(__file__).resolve().parents[1] / "src-tauri" / "src"
PIPELINE = SRC / "pipeline.rs"
COMMANDS = SRC / "commands.rs"
SETTINGS = SRC / "settings.rs"
EVENTS = Path(__file__).resolve().parents[1] / "src" / "lib" / "events.ts"
COORDINATOR = Path(__file__).resolve().parents[1] / "src" / "lib" / "batchOperationCoordinator.ts"


def _fn_body(text: str, signature: str) -> str:
    start = text.find(signature)
    if start == -1:
        raise AssertionError(f"{signature!r} not found — this gate would pass vacuously")
    rest = text[start + len(signature):]
    end = rest.find("\n    fn ")
    other = rest.find("\n    pub fn ")
    if other != -1 and (end == -1 or other < end):
        end = other
    return rest[:end if end != -1 else len(rest)]


def test_selecting_the_champion_cannot_be_overridden_by_the_small_model() -> None:
    """`should_use_wsl_primary_asr` must depend ONLY on the selected engine + a client script.

    The removed clause was `&& !(use_finetuned_asr && finetuned_model_paths().is_some())` — the exact
    line that silently handed 494 clips to the smaller model.
    """
    text = pipeline_surface(SRC)
    body = _fn_body(text, "fn should_use_wsl_primary_asr(&self) -> bool {")
    assert "use_finetuned_asr" not in body, (
        "should_use_wsl_primary_asr consults use_finetuned_asr again — selecting WSL7B must outrank "
        "the flag, or the champion is silently replaced by the smaller model"
    )
    assert "AsrModelSize::WSL7B" in body, "the gate no longer keys on the selected engine"


def test_champion_is_the_factory_default_and_auxiliary_models_default_off() -> None:
    """A reset/missing settings file must select only the 7B champion, never the bundled CTC model."""
    text = SETTINGS.read_text(encoding="utf-8")
    default_body = _fn_body(text, "fn default() -> Self {")
    assert "asr_model_size: AsrModelSize::WSL7B" in default_body, (
        "AppSettings default no longer selects the fine-tuned OmniASR-7B champion"
    )
    assert "asr_model_size: AsrModelSize::CTC300M" not in default_body, (
        "CTC-300M became an implicit app default; it must remain an explicit optional engine"
    )
    multi_default = _fn_body(text, "fn default_multi_engine_hypotheses() -> bool {")
    assert re.search(r"\bfalse\b", multi_default), (
        "auxiliary 300M/1B/MMS hypotheses must default off so only the champion runs"
    )
    assert "champion_supervision_enabled: false" in default_body, (
        "starting the app must not auto-allocate the champion's GPUs; lifecycle supervision is an "
        "explicit owner action"
    )


def test_legacy_auxiliary_flag_cannot_run_smaller_models_beside_champion() -> None:
    """Old settings may contain multi_engine_hypotheses=true; WSL7B must still remain single-engine."""
    text = pipeline_surface(SRC)
    helper = _fn_body(text, "fn auxiliary_hypotheses_enabled(settings: &AppSettings) -> bool {")
    assert "multi_engine_hypotheses" in helper, "shared auxiliary-model opt-in guard is missing"
    assert "AsrModelSize::WSL7B" in helper and "!=" in helper, (
        "legacy multi_engine=true can launch smaller models while the 7B champion is selected"
    )
    body = _fn_body(text, "fn populate_hypotheses_reusing_primary(")
    guard = body[: body.find("let model_dir")]
    assert "auxiliary_hypotheses_enabled(&self.settings)" in guard and "return Ok(())" in guard, (
        "hypothesis population bypasses the shared champion-only guard"
    )


def test_import_never_routes_through_scribe_or_cloud_fallback() -> None:
    """Cloud consent may reveal explicit tools, but it must never replace the champion during import."""
    text = pipeline_surface(SRC)
    body = _fn_body(text, "pub fn import_single_file_with_events(")
    # Do not search for the bare substring "scribe": ordinary words such as "Transcribe" contain it.
    assert "scribe_api" not in body.lower() and "elevenlabs" not in body.lower(), (
        "single-file import still contains an automatic Scribe/cloud primary path"
    )
    assert "import_single_file_via_scribe" not in text, (
        "the retired whole-file Scribe import helper remains callable"
    )


def test_champion_review_cannot_consume_stale_auxiliary_votes() -> None:
    """Legacy hypothesis rows must not influence review or trigger a multi-ASR jury in champion mode."""
    commands = command_surface(SRC)
    filter_body = _fn_body(commands, "fn hypotheses_for_selected_asr(")
    assert "AsrModelSize::WSL7B" in filter_body, "review hypothesis filtering is not selected-mode aware"
    # Pins the BEHAVIOUR, not a symbol name. The champion is content-addressed now, so the filter no
    # longer matches a fixed string — it matches the row's OWN producing model. That is the right unit
    # of provenance but re-admits what the fixed string excluded: a clip drafted by a weaker engine
    # BEFORE WSL7B was selected carries that engine's id. `recorded_model_is_champion` is the guard
    # that keeps such a row contributing NO auxiliary vote, and dropping it would silently restore the
    # 494/494 failure. Both halves are required.
    assert "recorded_model_is_champion" in filter_body and ".retain(" in filter_body, (
        "champion review does not discard historical 300M/1B/MMS/Scribe hypotheses"
    )
    assert "if !recorded_model_is_champion" in filter_body, (
        "the non-champion producer guard is gone — a weaker engine's stored draft would surface as an "
        "auxiliary vote during champion review"
    )

    jury_body = _fn_body(commands, "pub fn run_jury_pipeline_core_via(")
    guard = jury_body[: jury_body.find("let t1_threshold")]
    assert "AsrModelSize::WSL7B" in guard and '"mode": "not_required"' in guard, (
        "the automatic multi-ASR jury can still run while the sole 7B champion is selected"
    )

    pipeline = pipeline_surface(SRC)
    stage = _fn_body(pipeline, "fn multi_model_hypothesis_stage(")
    assert "AsrModelSize::WSL7B" in stage and '"not_required"' in stage, (
        "champion imports are still falsely blocked on optional multi-model coverage"
    )


def test_the_finetuned_override_yields_to_the_champion() -> None:
    """Every primary-drafter override goes through `finetuned_override_active`, which yields to WSL7B."""
    text = pipeline_surface(SRC)
    body = _fn_body(text, "fn finetuned_override_active(&self) -> bool {")
    assert "use_finetuned_asr" in body and "WSL7B" in body, (
        "finetuned_override_active must require the flag AND that the champion is not the selection"
    )
    assert "!=" in body, "the override must be DISABLED when WSL7B is selected, not enabled by it"

    # No primary-drafter branch may read the raw flag any more.
    for match in re.finditer(r"if self\.settings\.use_finetuned_asr\s*\{", text):
        line = text[: match.start()].count("\n") + 1
        raise AssertionError(
            f"pipeline.rs:{line} branches on the raw use_finetuned_asr flag; use "
            "finetuned_override_active() so selecting the champion outranks it"
        )


def test_batch_transcribe_hard_stops_on_the_first_failure() -> None:
    """A durable item failure must stop the loop and terminalize as halted, never tally-and-carry."""
    text = command_surface(SRC)
    db = (SRC / "db" / "batch_jobs.rs").read_text(encoding="utf-8")
    for required in (
        "transcribe_bound_draft_only",
        "BatchItemCommitOutcomeV1::Failed { code }",
        "BatchTerminalIntentV1::Failed { code }",
        "worker.finish(terminal_intent)",
        "BatchJobLifecycleV1::Failed => BatchRunDisposition::Halted",
        '"halted"',
        "batch_halt_error",
    ):
        assert required in text, f"durable batch hard-stop contract is missing: {required}"
    failed_arm = text.find("BatchItemCommitOutcomeV1::Failed { code }")
    assert failed_arm != -1 and re.search(r"break(?:\s+'[a-z_]+)?;", text[failed_arm:failed_arm + 400]), (
        "a durable failed item does not immediately stop later champion work"
    )
    for required in (
        "require_batch_not_hard_stopped_v1",
        "durable_item_failure_globally_stops_later_champion_and_normalize_effects",
    ):
        assert required in db, f"durable global hard-stop proof is missing: {required}"


def test_the_ui_surfaces_the_hard_stop_instead_of_swallowing_it() -> None:
    """The frontend half of the hard stop. Only the Rust emit was pinned — which is why this shipped.

    `batch_transcribe`'s terminal emit is `type: "halted"` + a typed `CommandErrorV1`, and it is the ONLY terminal
    event for the run. `events.ts` typed the union as started|progress|completed and branched on
    exactly those three, so "halted" matched nothing: the segment list never refreshed, isProcessing
    stayed true, batchProgress stayed 'running' and pipelinePhase stayed 'transcribing' forever, and
    the cause canon REQUIRES be reported was never shown. A hard stop the user cannot see is the
    silent fallback this whole policy exists to forbid.
    """
    text = EVENTS.read_text(encoding="utf-8")

    start = text.find("export interface BatchProgressEvent {")
    assert start != -1, "BatchProgressEvent is gone — this gate would pass vacuously"
    iface = text[start : text.index("\n}", start)]
    # The `type:` line itself, not the interface text: the explanatory comment above it also spells
    # 'halted', and a substring scan over the whole block passed while the union had lost the member.
    union = re.search(r"(?m)^\s*type:\s*(.+);$", iface)
    assert union and "'halted'" in union.group(1), "the BatchProgressEvent type union dropped 'halted'"
    assert "error?: CommandErrorV1" in iface, (
        "BatchProgressEvent no longer carries the typed halt cause"
    )
    assert "haltedBy" not in iface, "raw haltedBy prose was reintroduced into the renderer contract"

    coordinator = COORDINATOR.read_text(encoding="utf-8")
    notify_start = coordinator.find("function notifyRecoveredOutcome(")
    notify_end = coordinator.find("function notifyOutcomeUnknown(", notify_start)
    assert notify_start != -1 and notify_end != -1, "durable batch outcome presenter is missing"
    presenter = coordinator[notify_start:notify_end]
    assert "outcome.disposition === 'panicked' || outcome.disposition === 'halted'" in presenter
    assert "notifications.error" in presenter and "publicBatchHaltDetail" in presenter, (
        "a durable halted outcome must reach the owner as a localized error"
    )

    settle_start = coordinator.find("async function settle(")
    settle_end = coordinator.find("function scheduleStatusMonitor(", settle_start)
    assert settle_start != -1 and settle_end != -1, "exact durable settlement path is missing"
    settlement = coordinator[settle_start:settle_end]
    presented = settlement.find("notifyRecoveredOutcome(")
    refreshed = settlement.find("boundedBatchRefresh(")
    acknowledged = settlement.find("acknowledgeBatchRunWithRetry(")
    assert -1 not in (presented, refreshed, acknowledged) and presented < refreshed < acknowledged, (
        "the owner must see the durable outcome and refresh truth before the backend may forget it"
    )

    listen_start = text.find("stageListener<unknown>(generation, staged, 'batch-progress'")
    assert listen_start != -1, "the batch-progress listener is gone — this gate would pass vacuously"
    listener_end = text.find("'batch-worker-settled'", listen_start)
    assert listener_end != -1, "the post-worker settlement listener is gone"
    listener = text[listen_start:listener_end]
    assert "publicBatchProgressEvent(event.payload)" in listener, (
        "batch progress no longer passes through the closed runtime validator"
    )
    assert "'halted'" in listener and "markBatchTerminalEvent" in listener, (
        "the batch-progress listener ignores or misclassifies halted telemetry"
    )
    settled_start = text.find("stageListener<unknown>(generation, staged, 'batch-worker-settled'")
    assert settled_start != -1 and "onBatchWorkerSettled" in text[settled_start:settled_start + 1800], (
        "physical worker settlement no longer delegates to durable outcome reconciliation"
    )


def main() -> None:
    test_champion_is_the_factory_default_and_auxiliary_models_default_off()
    test_selecting_the_champion_cannot_be_overridden_by_the_small_model()
    test_legacy_auxiliary_flag_cannot_run_smaller_models_beside_champion()
    test_import_never_routes_through_scribe_or_cloud_fallback()
    test_champion_review_cannot_consume_stale_auxiliary_votes()
    test_the_finetuned_override_yields_to_the_champion()
    test_batch_transcribe_hard_stops_on_the_first_failure()
    test_the_ui_surfaces_the_hard_stop_instead_of_swallowing_it()
    print("champion supremacy + hard stop policy passed")


if __name__ == "__main__":
    main()
