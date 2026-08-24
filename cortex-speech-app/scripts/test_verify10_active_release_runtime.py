#!/usr/bin/env python3
"""Pins verify-10's immutable private-production runtime discovery."""

from __future__ import annotations

import hashlib
import importlib.util
import sys
import tempfile
from pathlib import Path


VERIFY = Path(__file__).resolve().parents[2] / "scripts" / "verify_10.py"
spec = importlib.util.spec_from_file_location("verify10_active_release_runtime", VERIFY)
assert spec is not None and spec.loader is not None
GATE = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = GATE
spec.loader.exec_module(GATE)

GIT_SHA = "a" * 40


def fixture() -> tuple[tempfile.TemporaryDirectory[str], Path, dict[str, object], Path]:
    temporary = tempfile.TemporaryDirectory()
    root = Path(temporary.name) / "releases"
    directory = root / "release-id"
    directory.mkdir(parents=True)
    exe = directory / "cortex-speech-app.exe"
    exe.write_bytes(b"prefix CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii") + b" suffix")
    manifest: dict[str, object] = {
        "schema": 1,
        "directory": str(directory),
        "appExe": str(exe),
        "appSha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
        "appGitSha": GIT_SHA,
    }
    return temporary, root, manifest, exe


def test_valid_hash_bound_immutable_release_is_selected() -> None:
    temporary, root, manifest, exe = fixture()
    try:
        assert GATE.validate_active_release_runtime(manifest, root) == exe.resolve()
    finally:
        temporary.cleanup()


def test_active_release_hash_drift_fails_closed() -> None:
    temporary, root, manifest, exe = fixture()
    try:
        exe.write_bytes(exe.read_bytes() + b"tampered")
        try:
            GATE.validate_active_release_runtime(manifest, root)
        except ValueError as error:
            assert "hash drifted" in str(error)
        else:
            raise AssertionError("tampered immutable release was accepted")
    finally:
        temporary.cleanup()


def test_active_release_path_escape_fails_closed() -> None:
    temporary, root, manifest, _exe = fixture()
    try:
        outside = Path(temporary.name) / "outside" / "cortex-speech-app.exe"
        outside.parent.mkdir()
        outside.write_bytes(b"CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii"))
        manifest["directory"] = str(outside.parent)
        manifest["appExe"] = str(outside)
        manifest["appSha256"] = hashlib.sha256(outside.read_bytes()).hexdigest()
        try:
            GATE.validate_active_release_runtime(manifest, root)
        except ValueError as error:
            assert "escapes" in str(error)
        else:
            raise AssertionError("release path outside the immutable root was accepted")
    finally:
        temporary.cleanup()


def test_active_release_manifest_git_sha_must_match_the_binary_marker() -> None:
    temporary, root, manifest, _exe = fixture()
    try:
        manifest["appGitSha"] = "b" * 40
        try:
            GATE.validate_active_release_runtime(manifest, root)
        except ValueError as error:
            assert "git SHA" in str(error)
        else:
            raise AssertionError("manifest/binary git mismatch was accepted")
    finally:
        temporary.cleanup()


if __name__ == "__main__":
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"verify-10 active release regressions passed ({len(tests)} assertions)")
