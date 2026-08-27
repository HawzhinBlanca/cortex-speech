#!/usr/bin/env python3
"""Prove that no Rust `#[ignore]` can disappear from release accounting."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import re
import subprocess
import sys
from contextlib import redirect_stdout
from unittest import mock
from pathlib import Path

import run_owner_rust_opt_ins as owner_runner


AUDIOBOOK_TESTS = owner_runner.AUDIOBOOK_TESTS
REAL_MEDIA_TESTS = owner_runner.REAL_MEDIA_TESTS
SCALE_TEST = owner_runner.SCALE_TEST


APP_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = APP_ROOT.parent
RUST_ROOT = APP_ROOT / "src-tauri"
MANIFEST_PATH = APP_ROOT / "docs" / "owner_rust_opt_in_tests.v1.json"
VERIFY_PATH = REPO_ROOT / "scripts" / "verify_10.py"
IGNORE_TEST = re.compile(
    r"(?m)^\s*#\[ignore(?:\s*=\s*[^\]]+)?\][^\r\n]*\r?\n"
    r"\s*(?:async\s+)?fn\s+([A-Za-z0-9_]+)\s*\("
)


def _observed() -> set[tuple[str, str]]:
    result: set[tuple[str, str]] = set()
    for path in sorted(RUST_ROOT.rglob("*.rs")):
        source = path.read_text(encoding="utf-8")
        relative = path.relative_to(REPO_ROOT).as_posix()
        for selector in IGNORE_TEST.findall(source):
            item = (relative, selector)
            if item in result:
                raise AssertionError(f"duplicate ignored Rust selector: {item}")
            result.add(item)
    return result


def _manifest() -> list[dict[str, str]]:
    document = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert document.get("schema") == 1
    tests = document.get("tests")
    assert isinstance(tests, list) and tests
    return tests


def _verifier_gate_profiles() -> dict[str, frozenset[str]]:
    spec = importlib.util.spec_from_file_location("opt_in_verify_10", VERIFY_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return {gate.id: gate.profiles for gate in module.GATES}


def test_manifest_exactly_covers_every_ignore() -> None:
    entries = _manifest()
    declared = {(entry["source"], entry["selector"]) for entry in entries}
    assert len(declared) == len(entries), "the opt-in manifest contains a duplicate selector"
    observed = _observed()
    assert declared == observed, (
        f"Rust opt-in manifest drift: missing={sorted(observed - declared)}, "
        f"stale={sorted(declared - observed)}"
    )


def test_every_scope_has_real_authority() -> None:
    gates = _verifier_gate_profiles()
    allowed_scopes = {"owner-product", "model-evidence", "diagnostic-tool"}
    for entry in _manifest():
        scope = entry["scope"]
        authority = entry["authority"]
        assert scope in allowed_scopes
        if scope == "owner-product":
            assert authority in gates, f"owner opt-in names missing verifier gate {authority}"
            assert "owner-product" in gates[authority], f"{authority} is not an owner gate"
        elif scope == "model-evidence":
            assert authority == "model-evidence"
        else:
            assert authority.startswith("non-certifying-")


def test_owner_runner_and_manifest_agree() -> None:
    routed = {
        entry["selector"]
        for entry in _manifest()
        if entry["authority"] == "owner-real-media-rust"
    }
    assert routed == {*REAL_MEDIA_TESTS, *AUDIOBOOK_TESTS}
    scale = {
        entry["selector"]
        for entry in _manifest()
        if entry["authority"] == "owner-scale-export-rust"
    }
    assert scale == {SCALE_TEST.split("::")[-1]}


def test_owner_runner_refuses_missing_proof_inputs() -> None:
    with mock.patch.dict(
        os.environ,
        {owner_runner.MEDIA_ENV: "", owner_runner.AUDIOBOOK_ENV: ""},
        clear=False,
    ):
        try:
            owner_runner._validate_media_inputs()
        except owner_runner.OwnerOptInError as error:
            assert owner_runner.MEDIA_ENV in str(error)
        else:
            raise AssertionError("missing owner media inputs must fail before Cargo starts")


def test_owner_runner_refuses_a_vacuous_ignored_test() -> None:
    fake = subprocess.CompletedProcess(
        args=["cargo"],
        returncode=0,
        stdout=(
            "fixture absent; skipping\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        ),
        stderr="",
    )
    with mock.patch.object(owner_runner.subprocess, "run", return_value=fake):
        with redirect_stdout(io.StringIO()):
            try:
                owner_runner._run_exact(["--test", "real_audio"], "example", os.environ.copy())
            except owner_runner.OwnerOptInError as error:
                assert "emitted skip output" in str(error)
            else:
                raise AssertionError("a skipped opt-in must never impersonate one passing test")


def main() -> None:
    test_manifest_exactly_covers_every_ignore()
    test_every_scope_has_real_authority()
    test_owner_runner_and_manifest_agree()
    test_owner_runner_refuses_missing_proof_inputs()
    test_owner_runner_refuses_a_vacuous_ignored_test()
    print(f"Rust opt-in manifest policy passed ({len(_manifest())} classified tests)")


if __name__ == "__main__":
    main()
