#!/usr/bin/env python3
"""Regression pins for champion-only standard fetch, bundle and release verification paths."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
from pathlib import Path
from unittest import mock

from _pipeline_policy_util import pipeline_surface
from _command_policy_util import command_surface


APP = Path(__file__).resolve().parent.parent
REPO = APP.parent
VERIFY = REPO / "scripts" / "verify_10.py"


def _load_fetch_models():
    path = APP / "scripts" / "fetch_models.py"
    spec = importlib.util.spec_from_file_location("cortex_fetch_models_policy", path)
    if spec is None or spec.loader is None:
        raise AssertionError("could not load fetch_models.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _resources(name: str) -> list[str]:
    config = json.loads((APP / "src-tauri" / name).read_text(encoding="utf-8"))
    return config.get("bundle", {}).get("resources", [])


def test_standard_bundles_contain_no_auxiliary_asr() -> None:
    expected = [
        "models/silero_vad_v4.onnx",
        "models/onnxruntime.dll/onnxruntime.dll",
        "models/onnxruntime.dll/onnxruntime_providers_shared.dll",
        "../scripts/cortex_7b_server.py",
        "../scripts/cortex_7b_client.py",
    ]
    forbidden = ("omniasr-ctc-300m", "omniasr-ctc-1b", "finetuned-mms", "scribe", "elevenlabs")
    for name in ("tauri.conf.json", "tauri.windows.conf.json"):
        resources = _resources(name)
        assert resources == expected, f"{name} standard resources drifted: {resources}"
        lowered = json.dumps(resources).lower()
        assert not any(term in lowered for term in forbidden), f"{name} bundles an auxiliary ASR"


def test_mms_bundle_override_is_diagnostic_only_and_never_used_by_release() -> None:
    path = APP / "src-tauri" / "tauri.finetuned.conf.json"
    source = path.read_text(encoding="utf-8")
    assert "EXPLICIT DIAGNOSTIC-ONLY" in source
    resources = _resources(path.name)
    assert "models/finetuned-mms-ckb/model.onnx" in resources
    assert "models/finetuned-mms-ckb/vocab.json" in resources
    assert not any("omniasr-ctc-" in item.lower() for item in resources)
    for workflow in (REPO / ".github" / "workflows").glob("*.yml"):
        text = workflow.read_text(encoding="utf-8")
        assert "--config src-tauri/tauri.finetuned.conf.json" not in text, (
            f"{workflow.name} invokes the diagnostic bundle"
        )


def test_standard_workflows_never_fetch_optional_asr() -> None:
    package = json.loads((APP / "package.json").read_text(encoding="utf-8"))
    assert package["scripts"]["fetch-models"] == "python scripts/fetch_models.py"
    assert package["scripts"]["verify-models"] == "python scripts/fetch_models.py --check"
    for name in ("ci.yml", "release.yml"):
        workflow = (REPO / ".github" / "workflows" / name).read_text(encoding="utf-8")
        assert "--include-optional-asr" not in workflow, f"{name} fetches an optional ASR"


def test_standard_fetch_selects_no_optional_asr_and_checks_any_present_group() -> None:
    fetch = _load_fetch_models()
    required = json.dumps(fetch.REQUIRED_ITEMS).lower()
    assert not any(term in required for term in ("omniasr", "mms", "scribe", "eleven"))
    assert set(fetch.OPTIONAL_ASR_ITEMS) == {"300m", "1b", "mms"}

    payload = b"pinned optional bytes"
    pin = hashlib.sha256(payload).hexdigest()
    optional = {"toy": [{"dest": "toy/model.bin", "sha256": pin, "url": "https://invalid.test/toy"}]}
    with (
        tempfile.TemporaryDirectory() as tmp,
        mock.patch.object(fetch, "MODELS_DIR", Path(tmp)),
        mock.patch.object(fetch, "REQUIRED_ITEMS", []),
        mock.patch.object(fetch, "OPTIONAL_ASR_ITEMS", optional),
        mock.patch.object(fetch, "NON_FETCHABLE_OPTIONAL_ASR", frozenset()),
    ):
        with mock.patch.object(fetch, "_download", side_effect=AssertionError("standard fetch touched optional ASR")):
            assert fetch.download() == 0
        model = Path(tmp) / "toy" / "model.bin"
        model.parent.mkdir(parents=True)
        model.write_bytes(b"wrong")
        assert fetch.check() == 1, "present hash-mismatched optional artifact was accepted"
        model.write_bytes(payload)
        assert fetch.check() == 0, "present pinned optional artifact should verify"


def test_optional_asr_download_requires_an_explicit_selection() -> None:
    fetch = _load_fetch_models()
    payload = b"explicit diagnostic artifact"
    pin = hashlib.sha256(payload).hexdigest()
    optional = {"toy": [{"dest": "toy/model.bin", "sha256": pin, "url": "https://invalid.test/toy"}]}
    with (
        tempfile.TemporaryDirectory() as tmp,
        mock.patch.object(fetch, "MODELS_DIR", Path(tmp)),
        mock.patch.object(fetch, "REQUIRED_ITEMS", []),
        mock.patch.object(fetch, "OPTIONAL_ASR_ITEMS", optional),
        mock.patch.object(fetch, "NON_FETCHABLE_OPTIONAL_ASR", frozenset()),
        mock.patch.object(fetch, "_download", return_value=payload) as downloader,
    ):
        assert fetch.download(frozenset({"toy"})) == 0
        downloader.assert_called_once()
        assert (Path(tmp) / "toy" / "model.bin").read_bytes() == payload


def test_verify10_standard_gates_are_champion_only() -> None:
    source = VERIFY.read_text(encoding="utf-8")
    gates = source[source.index("GATES = [") : source.index("# Charter DoD legs descoped")]
    for retired in ("rtf-bench", "ignored-real-model", "constrained-ipc-e2e", "finetuned-ipc-e2e"):
        assert f'("{retired}"' not in gates, f"verify_10 still auto-runs optional model gate {retired}"
    assert '("real-app-e2e"' in gates and '("pipeline-ipc-e2e"' in gates
    assert '("pipeline-ipc-e2e"' in gates and "_probe_champion_ipc_harness" in gates
    assert "--include-optional-asr" not in gates
    assert "CTC300M" not in gates and "CTC1B" not in gates


def test_real_harness_defaults_and_gate_mode_cannot_substitute_a_small_model() -> None:
    profile = (APP / "e2e_profile.cjs").read_text(encoding="utf-8")
    real = (APP / "e2e_real_app.cjs").read_text(encoding="utf-8")
    pipeline = (APP / "e2e_pipeline_ipc.cjs").read_text(encoding="utf-8")
    egress = (APP / "scripts" / "egress_probe.cjs").read_text(encoding="utf-8")
    latency = (APP / "scripts" / "latency_probe.cjs").read_text(encoding="utf-8")

    assert "async function provisionEngine(page, dataDir, engine = 'WSL7B')" in profile
    assert "champion_supervision_enabled = false" in profile
    assert "execFileSync" in profile and "model_versions" in profile
    assert "process.env.CORTEX_GATE === '1' ? 'WSL7B'" in real
    assert "provisionEngine(page, DATA_DIR);" in pipeline
    assert "process.env.CORTEX_EGRESS_TRANSCRIBE === '1'" in egress
    assert "provisionEngine(page, DATA_DIR, 'WSL7B')" in egress
    assert "s.asr_model_size = 'CTC300M'" not in egress
    assert "Get-CimInstance Win32_Process" in egress
    assert "app process tree" in egress
    assert "Why the MAIN exe PID only" not in egress
    assert "process.env.CORTEX_ASR_ENGINE || 'WSL7B'" in latency
    assert "provisionEngine(page, DATA_DIR, engine)" in latency


def test_shipped_model_management_is_support_only() -> None:
    commands = (APP / "src-tauri" / "src" / "commands" / "model_download.rs").read_text(encoding="utf-8")
    models = (APP / "src-tauri" / "src" / "models.rs").read_text(encoding="utf-8")
    lib = (APP / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    health = (APP / "src-tauri" / "src" / "health.rs").read_text(encoding="utf-8")

    assert "Ok(mm.production_status())" in commands
    assert "mm.downloadable_missing_production_models()" in commands
    assert "pub async fn models_download(" not in commands
    assert "commands::models_download," not in lib
    assert "missing_production_models()" in health
    assert "missing_optional_model_names()" not in health

    allowlist = (
        'const PRODUCTION_RUNTIME_MODEL_FILENAMES: &[&str] = '
        '&["silero_vad_v4.onnx", CAMPP_MODEL, DENOISER_MODEL];'
    )
    assert allowlist in models
    status = models[models.index("pub fn production_status") :]
    status = status[: status.index("\n    }\n}")]
    assert '"path"' not in status, "production model status leaks a local filesystem path"


def test_removed_mms_and_single_file_ipc_cannot_be_reintroduced() -> None:
    backend = command_surface(APP / "src-tauri" / "src")
    lib = (APP / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    frontend = (APP / "src" / "lib" / "commands.ts").read_text(encoding="utf-8")
    stats = (APP / "src" / "lib" / "StatsDashboard.svelte").read_text(encoding="utf-8")
    registry = (APP / "src" / "lib" / "ModelRegistry.svelte").read_text(encoding="utf-8").lower()

    for source in (backend, lib, frontend, stats):
        assert "verify_finetuned_model_integrity" not in source
    assert "verify-model-btn" not in stats
    assert "mms-ckb" not in registry and "scribe" not in registry


def test_registry_renderer_contract_never_returns_checkpoint_paths() -> None:
    backend = command_surface(APP / "src-tauri" / "src")
    frontend = (APP / "src" / "lib" / "commands.ts").read_text(encoding="utf-8")
    summary = backend[backend.index("pub struct ModelVersionSummary") : backend.index("impl From<crate::registry::ModelVersion>")]
    assert "checkpoint_path" not in summary
    assert "Result<Vec<ModelVersionSummaryV1>, crate::ipc_contract::CommandErrorV1>" in backend
    assert backend.count("Result<ModelVersionSummaryV1, crate::ipc_contract::CommandErrorV1>") >= 2
    assert "Result<ModelVersionSummary, String>" not in backend
    assert "checkpoint_path" not in frontend
    listing = backend[backend.index("pub fn list_model_versions") : backend.index("pub async fn import_model_checkpoint")]
    importing = backend[backend.index("pub async fn import_model_checkpoint") : backend.index("pub async fn import_model_deployment")]
    assert "version.family == crate::deployment::OMNIASR_7B_FAMILY" in listing
    assert "family: String" not in importing
    assert "crate::deployment::OMNIASR_7B_FAMILY" in importing


def test_every_production_writer_loads_champion_canon_or_is_retired() -> None:
    settings = (APP / "src-tauri" / "src" / "settings.rs").read_text(encoding="utf-8")
    desktop = (APP / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    importer = (APP / "src-tauri" / "src" / "bin" / "batch_importer.rs").read_text(encoding="utf-8")
    retired = (APP / "src-tauri" / "src" / "bin" / "batch_processor.rs").read_text(encoding="utf-8")

    assert "pub fn load_production(" in settings
    assert "settings.enforce_production_canon();" in settings
    assert "self.enforce_desktop_asr_canon();" in settings
    assert "self.enforce_advisory_cloud_canon();" in settings
    assert "AppSettings::load_production(&settings_path)" in desktop
    assert "AppSettings::load_production" in importer

    assert "HARD STOP" in retired
    for forbidden in ("Database", "AsrPool", "cortex-speech.db", "insert_segments", "AppSettings"):
        assert forbidden not in retired, f"retired batch_processor still reaches {forbidden}"


def test_production_importer_can_only_write_an_explicit_isolated_staging_profile() -> None:
    importer = (APP / "src-tauri" / "src" / "bin" / "batch_importer.rs").read_text(encoding="utf-8")
    assert "CORTEX_APP_DATA_DIR is required" in importer
    assert "live review imports are forbidden" in importer
    assert "cortex-batch-import-staging-profile" in importer
    assert "CORTEX_IMPORT_STAGING_TOKEN" in importer
    assert "sqlite_application_id_from_header" in importer
    assert "the import-staging sentinel is bound to a different canonical profile" in importer
    assert "--init-staging-profile" in importer
    assert 'std::env::var_os("CORTEX_APP_DATA_DIR")' in importer
    assert 'std::env::var_os("APPDATA")' not in importer


def test_shipped_gold_eval_has_no_auxiliary_engine_selector() -> None:
    lib = (APP / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    command = (APP / "src-tauri" / "src" / "commands" / "gold_eval.rs").read_text(encoding="utf-8")
    pipeline = pipeline_surface(APP / "src-tauri" / "src")
    frontend = (APP / "src" / "lib" / "commands.ts").read_text(encoding="utf-8")
    panel = (APP / "src" / "lib" / "RefineryPanel.svelte").read_text(encoding="utf-8")

    assert "commands::run_gold_eval_local," not in lib
    assert "pub async fn run_gold_eval_local(" not in command
    assert "runGoldEvalLocal" not in frontend
    assert "eval-local" not in panel and "Eval model id" not in panel
    shipped = pipeline[pipeline.index("pub fn run_gold_eval_asr") : pipeline.index("pub fn import_status_handle")]
    assert "run_wsl_segment_transcript" in shipped
    assert "require_exact_champion_result" in shipped
    assert "with_asr" not in shipped and "CTC300M" not in shipped and "CTC1B" not in shipped


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"CHAMPION RELEASE POLICY: {len(tests)} regressions passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
