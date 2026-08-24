#!/usr/bin/env python3
"""Fail-before tests for immutable release staging and schema-safe rollback decisions."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
SUBJECT = APP / "scripts" / "release_private_production.py"
SPEC = importlib.util.spec_from_file_location("private_release", SUBJECT)
assert SPEC and SPEC.loader
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def seed_source(root: Path) -> None:
    scripts = root / "scripts"
    (scripts / "ops").mkdir(parents=True)
    (scripts / "ops" / "cortex-watchdog.ps1").write_text("Write-Output 'watchdog'\n", encoding="utf-8")
    (scripts / "release_private_production.py").write_text("# controller\n", encoding="utf-8")
    migrations = root / "src-tauri" / "src" / "migrations"
    migrations.mkdir(parents=True)
    (migrations / "mod.rs").write_text("// migration ledger\n", encoding="utf-8")
    dedup = {
        "manifestSchema": 1,
        "summary": {"unconfirmedRiskGroups": 0},
    }
    dedup["manifestSha256"] = hashlib.sha256(
        json.dumps(dedup, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    (root / release.DEDUP_MANIFEST_FILE).write_text(json.dumps(dedup), encoding="utf-8")


def seed_candidate(root: Path, app: bytes = b"candidate-app", admin: bytes = b"candidate-admin") -> None:
    root.mkdir(parents=True)
    (root / "cortex-speech-app.exe").write_bytes(app)
    (root / "pool_admin.exe").write_bytes(admin)


def test_stage_is_atomic_versioned_and_hash_bound() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source = base / "source"
        candidate = base / "candidate"
        releases = base / "releases"
        seed_source(source)
        seed_candidate(candidate)
        git_sha = "a" * 40
        manifest = release.stage_release(candidate, source, releases, git_sha)
        final = Path(manifest["directory"])
        assert final.name == (
            f"{git_sha[:12]}-{hashlib.sha256(b'candidate-app').hexdigest()[:12]}-"
            f"{release.operations_bundle_sha256(source)[:12]}-{manifest['dedupManifestSha256'][:12]}"
        )
        assert manifest["appSha256"] == hashlib.sha256(b"candidate-app").hexdigest()
        assert manifest["poolAdminSha256"] == hashlib.sha256(b"candidate-admin").hexdigest()
        assert not list(releases.glob(".*.staging-*"))
        assert release.validate_manifest(json.loads((final / release.RELEASE_MANIFEST_FILE).read_text()), expected_root=releases)
        assert release.stage_release(candidate, source, releases, git_sha) == manifest


def test_operations_bundle_is_part_of_identity_and_tampering_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate)
        first = release.stage_release(candidate, source, releases, "d" * 40)
        (source / "scripts" / "release_private_production.py").write_text("# changed controller\n", encoding="utf-8")
        second = release.stage_release(candidate, source, releases, "d" * 40)
        assert first["releaseId"] != second["releaseId"]
        staged_controller = Path(second["directory"]) / "scripts" / "release_private_production.py"
        staged_controller.write_text("# tampered after publication\n", encoding="utf-8")
        try:
            release.validate_manifest(second, expected_root=releases)
        except release.ReleaseError as error:
            assert "operations bundle" in str(error)
        else:
            raise AssertionError("changed recovery/controller bytes must invalidate the immutable release")


def test_tampered_release_is_refused_and_never_replaced() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate)
        manifest = release.stage_release(candidate, source, releases, "b" * 40)
        app = Path(manifest["appExe"])
        app.write_bytes(b"tampered")
        try:
            release.stage_release(candidate, source, releases, "b" * 40)
        except release.ReleaseError as error:
            assert "SHA-256" in str(error)
        else:
            raise AssertionError("an immutable release with changed bytes must fail closed")
        assert app.read_bytes() == b"tampered", "staging must never overwrite or hide a changed release"


def test_candidate_inside_live_release_root_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases = base / "source", base / "releases"
        candidate = releases / "candidate"
        seed_source(source)
        seed_candidate(candidate)
        try:
            release.stage_release(candidate, source, releases, "c" * 40)
        except release.ReleaseError as error:
            assert "outside" in str(error)
        else:
            raise AssertionError("a build inside the live release root is not an isolated candidate")


def test_schema_rollback_policy_never_destroys_new_v64_work() -> None:
    assert release.rollback_policy(63, 64, 2, 2, None) == "restore-pre-migration"
    assert release.rollback_policy(63, 63, 2, 2, None) == "resume-pre-migration"
    assert release.rollback_policy(63, 64, 2, 3, None) == "preserve-v64"
    assert release.rollback_policy(64, 64, 20, 20, 64) == "binary-only"
    assert release.rollback_policy(64, 64, 20, 21, 64) == "binary-only"
    assert release.rollback_policy(64, 64, 20, 20, None) == "blocked"
    assert release.rollback_policy(63, 63, 2, 3, None) == "blocked"


def test_stop_app_targets_one_exact_executable_and_waits_for_exit() -> None:
    if os.name != "nt":
        return
    ping = Path(os.environ["WINDIR"]) / "System32" / "ping.exe"
    with tempfile.TemporaryDirectory() as raw:
        decoy = Path(raw) / "cortex-speech-app.exe"
        shutil.copy2(ping, decoy)
        process = subprocess.Popen(
            [str(decoy), "127.0.0.1", "-t"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            release.stop_app([decoy], force_after_seconds=1)
            assert process.wait(timeout=3) is not None
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)


def test_restore_preserves_failed_database_and_verifies_snapshot() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, snapshot = base / "data", base / "snapshot"
        data.mkdir()
        snapshot.mkdir()
        for path, version, marker in (
            (data / "cortex-speech.db", 64, "failed-v64"),
            (snapshot / "cortex-speech.db", 63, "known-good-v63"),
        ):
            connection = sqlite3.connect(path)
            connection.executescript(
                "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT);"
                f"INSERT INTO schema_migrations VALUES({version}, 'test');"
                "CREATE TABLE marker(value TEXT);"
            )
            connection.execute("INSERT INTO marker VALUES(?)", (marker,))
            connection.commit()
            connection.close()
        preserved = release.restore_database(snapshot, data, 63)
        assert release.database_schema(data / "cortex-speech.db") == 63
        assert release.database_schema(preserved) == 64
        connection = sqlite3.connect(data / "cortex-speech.db")
        assert connection.execute("SELECT value FROM marker").fetchone()[0] == "known-good-v63"
        connection.close()


def test_watchdog_and_server_pin_the_release_boundary() -> None:
    watchdog = (APP / "scripts" / "ops" / "cortex-watchdog.ps1").read_text(encoding="utf-8")
    couch = (APP / "src-tauri" / "src" / "couch.rs").read_text(encoding="utf-8")
    controller = SUBJECT.read_text(encoding="utf-8")
    assert release.POINTER_FILE in watchdog
    assert "Get-VerifiedActiveRelease" in watchdog
    assert "function Get-Sha256Hex" in watchdog
    assert "$actualSha = Get-Sha256Hex $check[0]" in watchdog
    assert "(Get-FileHash" not in watchdog
    assert release.WATCHDOG_TASK == "CortexPrivateProductionWatchdog"
    assert release.LEGACY_WATCHDOG_TASK == "CortexWatchdog"
    assert '"-TaskName",\n        WATCHDOG_TASK' in controller
    assert "Wait-Process -Id $left.Id -Timeout 10" in controller
    assert "Cortex app process did not stop after the force deadline" in controller
    assert "New-ScheduledTaskTrigger -AtLogOn -User $currentPrincipal" in watchdog
    assert "$clock = New-ScheduledTaskTrigger -Once" in watchdog
    assert "-Trigger @($logon, $clock)" in watchdog
    assert release.MAINTENANCE_FILE in couch
    probe = couch.index('if path == "/api/claim/probe"')
    maintenance = couch.index("if maintenance", probe)
    auth = couch.index("let authenticated", maintenance)
    assert probe < maintenance < auth, "only the non-mutating link probe may precede maintenance refusal"


def test_watchdog_refuses_a_malformed_active_pointer_before_probing_or_launching() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        data = Path(raw)
        (data / release.POINTER_FILE).write_text('{"schema":1}\n', encoding="utf-8")
        env = dict(os.environ, CORTEX_WATCHDOG_DATA_DIR=str(data), CORTEX_WATCHDOG_PORT="1")
        result = subprocess.run(
            [
                "powershell.exe",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(APP / "scripts" / "ops" / "cortex-watchdog.ps1"),
                "-DryRun",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PRIVATE PRODUCTION RELEASE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
