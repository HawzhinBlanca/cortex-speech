#!/usr/bin/env python3
"""Versioned, fail-closed Cortex private-production release handover.

The controller stages immutable binaries before downtime, migrates only behind a maintenance marker,
proves the live reviewer links without minting sessions, and exposes the candidate only after every
read-only production gate passes. A persistent journal plus a scheduled recovery arm makes an
interrupted handover recoverable after a process crash or reboot.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import socket
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

EXPECTED_SCHEMA = 63
POINTER_FILE = "active-private-production-release.json"
JOURNAL_FILE = "pending-private-production-release.json"
MAINTENANCE_FILE = "private-production-maintenance.json"
RELEASE_MANIFEST_FILE = "release-manifest.json"
LEGACY_WATCHDOG_TASK = "CortexWatchdog"
WATCHDOG_TASK = "CortexPrivateProductionWatchdog"
RESTORE_TASK = "CortexDailyRestoreDrill"
RECOVERY_TASK = "CortexReleaseRecovery"
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA64 = re.compile(r"^[0-9a-f]{64}$")
MANIFEST_FIELDS = {
    "schema",
    "releaseId",
    "expectedDatabaseSchema",
    "appGitSha",
    "createdAtUtc",
    "directory",
    "appExe",
    "poolAdminExe",
    "appSha256",
    "poolAdminSha256",
    "watchdogScript",
    "watchdogSha256",
    "operationsSha256",
}
PROFILE_STATE = (
    "settings.json",
    "champion.json",
    "reviewer_dialects.json",
    "voice_focus.json",
    "review_pilot_policy.json",
)


class ReleaseError(RuntimeError):
    pass


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (OSError, ValueError) as error:
        raise ReleaseError(f"cannot read {path.name}: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseError(f"{path.name} must contain one JSON object")
    return value


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.parent / f".{path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    payload = (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")
    try:
        with temporary.open("xb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def is_within(child: Path, parent: Path) -> bool:
    try:
        child.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def validate_artifact(path: Path, label: str) -> Path:
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or resolved.is_symlink():
        raise ReleaseError(f"{label} must be a regular non-symlink file: {resolved}")
    if resolved.stat().st_size <= 0:
        raise ReleaseError(f"{label} is empty: {resolved}")
    return resolved


def operations_bundle_sha256(root: Path) -> str:
    """Bind every staged operational script plus the canonical migration ledger."""
    files = [
        path
        for path in (root / "scripts").rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.suffix.lower() != ".pyc"
    ]
    migrations = root / "src-tauri" / "src" / "migrations" / "mod.rs"
    if not migrations.is_file() or not files:
        raise ReleaseError("operations bundle is missing scripts or the canonical migration ledger")
    files.append(migrations)
    digest = hashlib.sha256()
    for path in sorted(files, key=lambda value: value.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content_sha = sha256_file(path).encode("ascii")
        digest.update(len(relative).to_bytes(4, "big"))
        digest.update(relative)
        digest.update(path.stat().st_size.to_bytes(8, "big"))
        digest.update(content_sha)
    return digest.hexdigest()


def validate_manifest(value: dict[str, Any], *, expected_root: Path | None = None) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReleaseError("release manifest must be one JSON object")
    if set(value) != MANIFEST_FIELDS:
        raise ReleaseError(
            f"release manifest fields are invalid (missing={sorted(MANIFEST_FIELDS - set(value))}, "
            f"extra={sorted(set(value) - MANIFEST_FIELDS)})"
        )
    if type(value["schema"]) is not int or value["schema"] != 1:
        raise ReleaseError("release manifest schema must be integer 1")
    if type(value["expectedDatabaseSchema"]) is not int or value["expectedDatabaseSchema"] != EXPECTED_SCHEMA:
        raise ReleaseError(f"release manifest must require database schema {EXPECTED_SCHEMA}")
    if not isinstance(value["appGitSha"], str) or not SHA40.fullmatch(value["appGitSha"]):
        raise ReleaseError("release manifest appGitSha is invalid")
    directory = Path(str(value["directory"])).resolve(strict=True)
    if expected_root is not None and not is_within(directory, expected_root):
        raise ReleaseError("release directory escapes the configured immutable release root")
    for path_field, hash_field in (
        ("appExe", "appSha256"),
        ("poolAdminExe", "poolAdminSha256"),
        ("watchdogScript", "watchdogSha256"),
    ):
        artifact = validate_artifact(Path(str(value[path_field])), path_field)
        if not is_within(artifact, directory):
            raise ReleaseError(f"{path_field} escapes the immutable release directory")
        expected = value[hash_field]
        if not isinstance(expected, str) or not SHA64.fullmatch(expected):
            raise ReleaseError(f"{hash_field} is invalid")
        if sha256_file(artifact) != expected:
            raise ReleaseError(f"{path_field} does not match its release SHA-256")
    operations_sha = value["operationsSha256"]
    if not isinstance(operations_sha, str) or not SHA64.fullmatch(operations_sha):
        raise ReleaseError("operationsSha256 is invalid")
    if operations_bundle_sha256(directory) != operations_sha:
        raise ReleaseError("the staged operations bundle does not match its release SHA-256")
    return value


def copy_source_bundle(source_root: Path, stage: Path) -> None:
    scripts = source_root / "scripts"
    migrations = source_root / "src-tauri" / "src" / "migrations" / "mod.rs"
    if not scripts.is_dir() or not migrations.is_file():
        raise ReleaseError("source root is missing scripts or the canonical migration ledger")
    shutil.copytree(
        scripts,
        stage / "scripts",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    migration_target = stage / "src-tauri" / "src" / "migrations"
    migration_target.mkdir(parents=True)
    shutil.copy2(migrations, migration_target / "mod.rs")


def stage_release(candidate_dir: Path, source_root: Path, release_root: Path, git_sha: str) -> dict[str, Any]:
    if not SHA40.fullmatch(git_sha):
        raise ReleaseError("--git-sha must be the exact lowercase 40-character release commit")
    candidate_dir = candidate_dir.resolve(strict=True)
    source_root = source_root.resolve(strict=True)
    release_root = release_root.resolve() if release_root.exists() else release_root.absolute()
    if is_within(candidate_dir, release_root):
        raise ReleaseError("candidate build must be outside the live immutable release root")
    app_source = validate_artifact(candidate_dir / "cortex-speech-app.exe", "candidate app")
    admin_source = validate_artifact(candidate_dir / "pool_admin.exe", "candidate pool_admin")
    app_sha = sha256_file(app_source)
    admin_sha = sha256_file(admin_source)
    operations_sha = operations_bundle_sha256(source_root)
    release_id = f"{git_sha[:12]}-{app_sha[:12]}-{operations_sha[:12]}"
    final = release_root / release_id
    if final.exists():
        manifest = validate_manifest(load_json(final / RELEASE_MANIFEST_FILE), expected_root=release_root)
        if manifest["releaseId"] != release_id or manifest["appSha256"] != app_sha or manifest["poolAdminSha256"] != admin_sha:
            raise ReleaseError(f"existing immutable release {release_id} does not match this candidate")
        return manifest

    release_root.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=f".{release_id}.staging-", dir=release_root))
    try:
        shutil.copy2(app_source, stage / "cortex-speech-app.exe")
        shutil.copy2(admin_source, stage / "pool_admin.exe")
        copy_source_bundle(source_root, stage)
        if operations_bundle_sha256(stage) != operations_sha:
            raise ReleaseError("staged operations bundle changed while it was copied")
        watchdog = stage / "scripts" / "ops" / "cortex-watchdog.ps1"
        validate_artifact(watchdog, "watchdog script")
        manifest = {
            "schema": 1,
            "releaseId": release_id,
            "expectedDatabaseSchema": EXPECTED_SCHEMA,
            "appGitSha": git_sha,
            "createdAtUtc": utc_now(),
            "directory": str(final),
            "appExe": str(final / "cortex-speech-app.exe"),
            "poolAdminExe": str(final / "pool_admin.exe"),
            "appSha256": app_sha,
            "poolAdminSha256": admin_sha,
            "watchdogScript": str(final / "scripts" / "ops" / "cortex-watchdog.ps1"),
            "watchdogSha256": sha256_file(watchdog),
            "operationsSha256": operations_sha,
        }
        atomic_json(stage / RELEASE_MANIFEST_FILE, manifest)
        stage.rename(final)
        return validate_manifest(load_json(final / RELEASE_MANIFEST_FILE), expected_root=release_root)
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def database_schema(db_path: Path) -> int:
    try:
        connection = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True, timeout=30)
        try:
            row = connection.execute("SELECT MAX(version) FROM schema_migrations").fetchone()
            if row is None or type(row[0]) is not int:
                raise ReleaseError("database has no authoritative schema migration history")
            return int(row[0])
        finally:
            connection.close()
    except sqlite3.Error as error:
        raise ReleaseError(f"database schema cannot be read: {error}") from error


def max_pool_decision_id(db_path: Path) -> int:
    connection = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True, timeout=30)
    try:
        exists = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='review_pool_decisions'"
        ).fetchone()
        if exists is None:
            return 0
        return int(connection.execute("SELECT COALESCE(MAX(id), 0) FROM review_pool_decisions").fetchone()[0])
    finally:
        connection.close()


def run(command: list[str], *, timeout: int = 300, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, capture_output=True, text=True, errors="replace", timeout=timeout, env=env)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise ReleaseError(f"command failed ({result.returncode}): {command[0]} {command[1] if len(command) > 1 else ''}: {detail[:2000]}")
    return result


def run_json(command: list[str], *, timeout: int = 300) -> dict[str, Any]:
    result = run(command, timeout=timeout)
    try:
        value = json.loads(result.stdout, object_pairs_hook=reject_duplicate_keys)
    except ValueError as error:
        raise ReleaseError(f"command returned invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseError("command did not return one JSON object")
    return value


def sqlite_backup(source: Path, destination: Path) -> None:
    src = sqlite3.connect(f"file:{source.as_posix()}?mode=ro", uri=True, timeout=30)
    dst = sqlite3.connect(destination)
    try:
        src.backup(dst, pages=4096, sleep=0.001)
        dst.execute("PRAGMA journal_mode=DELETE")
    finally:
        dst.close()
        src.close()


def preflight_clone(data_dir: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    db_path = data_dir / "cortex-speech.db"
    with tempfile.TemporaryDirectory(prefix="cortex-release-preflight-") as raw:
        clone = Path(raw)
        sqlite_backup(db_path, clone / "cortex-speech.db")
        for name in PROFILE_STATE:
            source = data_dir / name
            if source.is_file():
                shutil.copy2(source, clone / name)
        admin = str(manifest["poolAdminExe"])
        run_json([admin, "migrate", "--db", str(clone / "cortex-speech.db")], timeout=300)
        rights = run_json([admin, "stamp-rights", "--db", str(clone / "cortex-speech.db")], timeout=300)
        report = run_json([admin, "certify", "--db", str(clone / "cortex-speech.db"), "--full-integrity"], timeout=600)
        if report.get("appGitSha") != manifest["appGitSha"]:
            raise ReleaseError("candidate pool_admin is not built from the declared release commit")
        if report.get("databaseSchemaVersion") != EXPECTED_SCHEMA:
            raise ReleaseError("candidate did not migrate the live-sized clone to schema 63")
        if report.get("database", {}).get("healthy") is not True:
            raise ReleaseError("candidate clone database certification failed")
        if report.get("audio", {}).get("allAvailable") is not True:
            raise ReleaseError("candidate clone has missing or changed pool audio")
        if report.get("rights", {}).get("allExact") is not True:
            raise ReleaseError("candidate clone did not establish exact owner rights")
        return {"rights": rights, "certification": report}


def powershell_file(path: Path, *arguments: str, timeout: int = 300) -> None:
    run(
        ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(path), *arguments],
        timeout=timeout,
    )


def task_change(name: str, enabled: bool, *, allow_missing: bool = False) -> None:
    result = subprocess.run(
        ["schtasks.exe", "/change", "/tn", name, "/enable" if enabled else "/disable"],
        capture_output=True,
        text=True,
        errors="replace",
    )
    if result.returncode != 0 and not allow_missing:
        raise ReleaseError(f"could not {'enable' if enabled else 'disable'} {name}: {(result.stderr or result.stdout).strip()}")


def unregister_task(name: str) -> None:
    subprocess.run(
        ["schtasks.exe", "/delete", "/tn", name, "/f"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def stop_app(executables: list[Path], *, force_after_seconds: int = 8) -> None:
    targets = {str(path.resolve()).lower() for path in executables if path and path.exists()}
    if not targets:
        return
    # Windows PowerShell 5 wraps a one-element JSON array as one nested Object[], so `-contains`
    # compares an array object to the process path and silently matches nothing. Windows paths cannot
    # contain newlines; a newline-delimited exact-path list stays flat in every supported PowerShell.
    env = dict(os.environ, CORTEX_RELEASE_TARGETS="\n".join(sorted(targets)))
    script = r"""
$targets = @($env:CORTEX_RELEASE_TARGETS -split "`n" | Where-Object { $_ })
$processes = @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and ($targets -contains $_.Path.ToLowerInvariant())
})
foreach ($process in $processes) { [void]$process.CloseMainWindow() }
if ($processes.Count) { Wait-Process -Id $processes.Id -Timeout $env:CORTEX_RELEASE_STOP_TIMEOUT -ErrorAction SilentlyContinue }
$left = @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and ($targets -contains $_.Path.ToLowerInvariant())
})
foreach ($process in $left) { Stop-Process -Id $process.Id -Force }
if ($left.Count) { Wait-Process -Id $left.Id -Timeout 10 -ErrorAction SilentlyContinue }
$survivors = @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and ($targets -contains $_.Path.ToLowerInvariant())
})
if ($survivors.Count) { throw "Cortex app process did not stop after the force deadline" }
"""
    env["CORTEX_RELEASE_STOP_TIMEOUT"] = str(force_after_seconds)
    run(["powershell.exe", "-NoProfile", "-Command", script], timeout=force_after_seconds + 30, env=env)


def launch_app(path: Path) -> None:
    flags = 0
    if os.name == "nt":
        flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
    subprocess.Popen(
        [str(path)],
        cwd=path.parent,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        close_fds=True,
        creationflags=flags,
    )


def wait_for_server(port: int, timeout_seconds: int = 45) -> None:
    deadline = time.monotonic() + timeout_seconds
    context = ssl._create_unverified_context()
    last_error = "no response"
    while time.monotonic() < deadline:
        for scheme in ("https", "http"):
            try:
                request = urllib.request.Request(f"{scheme}://127.0.0.1:{port}/", method="GET")
                with urllib.request.urlopen(request, timeout=3, context=context if scheme == "https" else None):
                    return
            except urllib.error.HTTPError:
                return
            except (OSError, urllib.error.URLError, ssl.SSLError, TimeoutError) as error:
                last_error = str(error)
        time.sleep(0.5)
    raise ReleaseError(f"candidate did not answer on localhost:{port} within {timeout_seconds}s ({last_error})")


def session_reviewers(data_dir: Path) -> list[str]:
    session = load_json(data_dir / "couch_session.json")
    reviewers = session.get("reviewers")
    if not isinstance(reviewers, dict) or not reviewers:
        raise ReleaseError("the durable couch session contains no reviewer links")
    names = list(reviewers.values())
    if not all(isinstance(name, str) and name.strip() for name in names):
        raise ReleaseError("the durable couch session contains an invalid reviewer identity")
    canonical = [str(name).strip() for name in names]
    if len({name.lower() for name in canonical}) != len(canonical):
        raise ReleaseError("the durable couch session contains duplicate reviewer identities")
    return sorted(canonical, key=str.lower)


def reviewer_dialects(data_dir: Path, reviewer: str) -> list[str]:
    path = data_dir / "reviewer_dialects.json"
    if not path.is_file():
        return []
    value = load_json(path)
    matches = [item for name, item in value.items() if not name.startswith("_") and name.strip().lower() == reviewer.lower()]
    if len(matches) > 1 or (matches and not isinstance(matches[0], list)):
        raise ReleaseError(f"dialect policy for {reviewer} is ambiguous or invalid")
    if not matches:
        return []
    dialects = matches[0]
    if not all(isinstance(item, str) and item.strip().lower() in {"hawleri", "sorani", "badini"} for item in dialects):
        raise ReleaseError(f"dialect policy for {reviewer} is invalid")
    return list(dict.fromkeys(str(item).strip().lower() for item in dialects))


def prove_canonical_queues(data_dir: Path, manifest: dict[str, Any]) -> dict[str, int]:
    db = data_dir / "cortex-speech.db"
    available: dict[str, int] = {}
    for reviewer in session_reviewers(data_dir):
        dialects = reviewer_dialects(data_dir, reviewer)
        probe_command = [str(manifest["poolAdminExe"]), "probe", "--db", str(db), "--reviewer", reviewer]
        for dialect in dialects:
            probe_command.extend(["--dialect", dialect])
        probe = run_json(probe_command, timeout=180)
        count = probe.get("availableClips")
        if type(count) is not int or count <= 0:
            raise ReleaseError(f"canonical v63 queue is empty for reviewer {reviewer}")
        if (
            probe.get("passes") is not True
            or probe.get("sampleAudioValidWav") is not True
            or probe.get("submissionIdempotencyAuthority") is not True
        ):
            raise ReleaseError(f"canonical v63 audio/idempotency probe failed for reviewer {reviewer}")
        benchmark_command = [
            str(manifest["poolAdminExe"]),
            "benchmark",
            "--db",
            str(db),
            "--reviewer",
            reviewer,
            "--iterations",
            "3",
        ]
        for dialect in dialects:
            benchmark_command.extend(["--dialect", dialect])
        benchmark = run_json(benchmark_command, timeout=180)
        if benchmark.get("passes") is not True:
            raise ReleaseError(f"canonical v63 queue latency failed for reviewer {reviewer}")
        available[reviewer] = count
    return available


def prove_links(data_dir: Path, manifest: dict[str, Any], *, funnel: bool) -> None:
    script = Path(str(manifest["directory"])) / "scripts" / "check_reviewer_links_live.py"
    command = [sys.executable, str(script), "--data-dir", str(data_dir), "--require-private-production"]
    command.append("--funnel" if funnel else "--base-url")
    if not funnel:
        command.append("https://127.0.0.1:8737")
    run(command, timeout=180)


def certify_live(data_dir: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    report = run_json(
        [
            str(manifest["poolAdminExe"]),
            "certify",
            "--db",
            str(data_dir / "cortex-speech.db"),
            "--full-integrity",
            "--require-review-ready",
        ],
        timeout=600,
    )
    if report.get("appGitSha") != manifest["appGitSha"]:
        raise ReleaseError("live certification came from a different release commit")
    if report.get("rights", {}).get("allExact") is not True:
        raise ReleaseError("live pool rights are incomplete or conflicting")
    if report.get("audio", {}).get("allAvailable") is not True:
        raise ReleaseError("live pool audio is incomplete or changed")
    return report


def snapshot_before_handover(data_dir: Path, manifest: dict[str, Any]) -> Path:
    script = Path(str(manifest["directory"])) / "scripts" / "create_recovery_snapshot.py"
    label = f"preprivate_v{database_schema(data_dir / 'cortex-speech.db')}_to_v{EXPECTED_SCHEMA}"
    result = run([sys.executable, str(script), "--data-dir", str(data_dir), "--label", label], timeout=600)
    local = next((line.split("=", 1)[1].strip() for line in result.stdout.splitlines() if line.startswith("LOCAL_SNAPSHOT=")), None)
    if not local:
        raise ReleaseError("pre-handover snapshot command did not report a local snapshot")
    snapshot = Path(local).resolve(strict=True)
    if not (snapshot / "cortex-speech.db").is_file() or not (snapshot / "SNAPSHOT_MANIFEST.json").is_file():
        raise ReleaseError("pre-handover snapshot is incomplete")
    return snapshot


def restore_database(snapshot: Path, data_dir: Path, expected_schema: int) -> Path:
    source = snapshot / "cortex-speech.db"
    if database_schema(source) != expected_schema:
        raise ReleaseError("rollback snapshot schema does not match the pre-handover database")
    live = data_dir / "cortex-speech.db"
    temporary = data_dir / f".cortex-speech.rollback.{os.getpid()}.{time.time_ns()}.db"
    sqlite_backup(source, temporary)
    if database_schema(temporary) != expected_schema:
        temporary.unlink(missing_ok=True)
        raise ReleaseError("staged rollback database failed schema verification")
    check = sqlite3.connect(temporary)
    try:
        if check.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
            raise ReleaseError("staged rollback database failed integrity_check")
        if check.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise ReleaseError("staged rollback database has foreign-key violations")
    finally:
        check.close()
    quarantine = data_dir / "recovery-quarantine"
    quarantine.mkdir(exist_ok=True)
    preserved = quarantine / f"cortex-speech.failed-v{database_schema(live)}.{int(time.time())}.db"
    for sidecar in (Path(str(live) + "-wal"), Path(str(live) + "-shm")):
        sidecar.unlink(missing_ok=True)
    os.replace(live, preserved)
    os.replace(temporary, live)
    return preserved


def rollback_policy(source_schema: int, current_schema: int, baseline_id: int, current_id: int, previous_schema: int | None) -> str:
    if current_id > baseline_id:
        if current_schema == EXPECTED_SCHEMA and previous_schema == EXPECTED_SCHEMA:
            return "binary-only"
        return "preserve-v63" if current_schema == EXPECTED_SCHEMA else "blocked"
    if source_schema < EXPECTED_SCHEMA and current_schema == EXPECTED_SCHEMA:
        return "restore-pre-migration"
    if source_schema < EXPECTED_SCHEMA and current_schema == source_schema:
        return "resume-pre-migration"
    if source_schema == EXPECTED_SCHEMA and previous_schema == EXPECTED_SCHEMA:
        return "binary-only"
    return "blocked"


def write_maintenance(data_dir: Path, release_id: str) -> None:
    atomic_json(
        data_dir / MAINTENANCE_FILE,
        {"schema": 1, "releaseId": release_id, "startedAtUtc": utc_now(), "reviewWritesBlocked": True},
    )


def active_pointer(data_dir: Path, release_root: Path) -> dict[str, Any] | None:
    path = data_dir / POINTER_FILE
    return validate_manifest(load_json(path), expected_root=release_root) if path.is_file() else None


def register_release_tasks(manifest: dict[str, Any]) -> None:
    root = Path(str(manifest["directory"]))
    powershell_file(
        root / "scripts" / "ops" / "cortex-watchdog.ps1",
        "-Register",
        "-TaskName",
        WATCHDOG_TASK,
    )
    powershell_file(root / "scripts" / "ops" / "cortex-daily-restore-drill.ps1", "-Register")


def recover(data_dir: Path, release_root: Path) -> bool:
    journal_path = data_dir / JOURNAL_FILE
    if not journal_path.is_file():
        unregister_task(RECOVERY_TASK)
        return True
    journal = load_json(journal_path)
    candidate = validate_manifest(journal["candidate"], expected_root=release_root)
    previous = journal.get("previousActive")
    if previous is not None:
        if not isinstance(previous, dict):
            raise ReleaseError("release journal previousActive is invalid")
        previous = validate_manifest(previous, expected_root=release_root)
    source_schema = int(journal["sourceSchema"])
    baseline = int(journal["baselinePoolDecisionId"])
    db = data_dir / "cortex-speech.db"
    current_schema = database_schema(db)
    current_id = max_pool_decision_id(db)
    mode = rollback_policy(
        source_schema,
        current_schema,
        baseline,
        current_id,
        int(previous["expectedDatabaseSchema"]) if previous else None,
    )
    write_maintenance(data_dir, str(candidate["releaseId"]))
    task_change(WATCHDOG_TASK, False, allow_missing=True)
    task_change(LEGACY_WATCHDOG_TASK, False, allow_missing=True)
    stop_paths = [Path(str(candidate["appExe"]))]
    if previous:
        stop_paths.append(Path(str(previous["appExe"])))
    fallback_app = Path(str(journal["fallbackApp"])) if journal.get("fallbackApp") else None
    if fallback_app:
        stop_paths.append(fallback_app)
    stop_app(stop_paths)

    if mode in {"restore-pre-migration", "resume-pre-migration"}:
        preserved: Path | None = None
        if mode == "restore-pre-migration":
            snapshot = Path(str(journal.get("snapshotDir", ""))).resolve(strict=True)
            preserved = restore_database(snapshot, data_dir, source_schema)
        (data_dir / POINTER_FILE).unlink(missing_ok=True)
        (data_dir / MAINTENANCE_FILE).unlink(missing_ok=True)
        fallback_watchdog = Path(str(journal.get("fallbackWatchdog", "")))
        if not fallback_watchdog.is_file():
            raise ReleaseError("pre-migration rollback has no verified fallback watchdog")
        unregister_task(WATCHDOG_TASK)
        # The pre-managed watchdog may be an administrator-owned task. The logged-on reviewer
        # account can safely disable/enable it but cannot replace its action. Re-enable the proven
        # legacy task instead of trying to overwrite protected Task Scheduler authority.
        task_change(LEGACY_WATCHDOG_TASK, True)
        if fallback_app is None or not fallback_app.is_file():
            raise ReleaseError("pre-migration rollback has no verified fallback app")
        launch_app(fallback_app)
        wait_for_server(8737)
        prove_links(data_dir, candidate, funnel=False)
        prove_links(data_dir, candidate, funnel=True)
        journal_path.unlink(missing_ok=True)
        unregister_task(RECOVERY_TASK)
        if preserved is None:
            print(f"RELEASE RECOVERY: resumed unchanged schema v{source_schema} fallback")
        else:
            print(f"RELEASE RECOVERY: restored schema v{source_schema}; failed v63 database preserved at {preserved}")
        return True

    target = previous if mode == "binary-only" else candidate if mode == "preserve-v63" else None
    if target is None:
        raise ReleaseError(
            "automatic rollback is blocked: restoring an older database could destroy reviewer work, "
            "and no schema-63-compatible last-known-good release is available"
        )
    atomic_json(data_dir / POINTER_FILE, target)
    launch_app(Path(str(target["appExe"])))
    wait_for_server(8737)
    certify_live(data_dir, target)
    prove_links(data_dir, target, funnel=False)
    prove_links(data_dir, target, funnel=True)
    prove_canonical_queues(data_dir, target)
    register_release_tasks(target)
    task_change(WATCHDOG_TASK, True)
    task_change(LEGACY_WATCHDOG_TASK, False, allow_missing=True)
    (data_dir / MAINTENANCE_FILE).unlink(missing_ok=True)
    journal_path.unlink(missing_ok=True)
    unregister_task(RECOVERY_TASK)
    print(f"RELEASE RECOVERY: activated schema-63-compatible release {target['releaseId']} without restoring the database")
    return True


def deploy(args: argparse.Namespace) -> int:
    if os.name != "nt":
        raise ReleaseError("private-production deployment is supported only on the Windows review host")
    data_dir = args.data_dir.resolve(strict=True)
    release_root = args.release_root.resolve() if args.release_root.exists() else args.release_root.absolute()
    db = data_dir / "cortex-speech.db"
    if not db.is_file():
        raise ReleaseError(f"live database is missing: {db}")
    if (data_dir / JOURNAL_FILE).exists():
        raise ReleaseError("a prior release handover is unfinished; run the recover command first")
    source_schema = database_schema(db)
    if source_schema not in {62, EXPECTED_SCHEMA}:
        raise ReleaseError(f"deployment accepts only the proven v62->v63 or v63->v63 path, not schema v{source_schema}")
    session_reviewers(data_dir)
    previous = active_pointer(data_dir, release_root)
    if source_schema < EXPECTED_SCHEMA and previous is not None:
        raise ReleaseError("a schema-62 database cannot be bound to a schema-63 active release pointer")
    if source_schema == EXPECTED_SCHEMA and previous is None:
        raise ReleaseError("a schema-63 deployment requires a versioned last-known-good active release")
    manifest = stage_release(args.candidate_dir, args.source_root, release_root, args.git_sha)
    print(f"STAGED_RELEASE={manifest['releaseId']}")
    preflight = preflight_clone(data_dir, manifest)
    audio = preflight["certification"]["audio"]
    print(
        "PREFLIGHT_CLONE=PASS "
        f"schema={preflight['certification']['databaseSchemaVersion']} "
        f"audioClips={audio['clips'] - audio['missingClips']}/{audio['clips']} rightsExact=true"
    )
    if args.stage_only:
        return 0

    fallback_app = args.fallback_app.resolve(strict=True) if args.fallback_app else None
    fallback_watchdog = args.fallback_watchdog.resolve(strict=True) if args.fallback_watchdog else None
    if previous is None and (fallback_app is None or fallback_watchdog is None):
        raise ReleaseError("the first managed v62->v63 deployment requires --fallback-app and --fallback-watchdog")

    baseline = max_pool_decision_id(db)
    journal: dict[str, Any] = {
        "schema": 1,
        "phase": "prepared",
        "startedAtUtc": utc_now(),
        "sourceSchema": source_schema,
        "baselinePoolDecisionId": baseline,
        "candidate": manifest,
        "previousActive": previous,
        "fallbackApp": str(fallback_app) if fallback_app else None,
        "fallbackWatchdog": str(fallback_watchdog) if fallback_watchdog else None,
        "snapshotDir": None,
    }
    atomic_json(data_dir / JOURNAL_FILE, journal)
    write_maintenance(data_dir, str(manifest["releaseId"]))
    journal["phase"] = "maintenance"
    atomic_json(data_dir / JOURNAL_FILE, journal)
    recovery = Path(str(manifest["directory"])) / "scripts" / "ops" / "cortex-release-recovery.ps1"
    powershell_file(recovery, "-Register")
    task_change(LEGACY_WATCHDOG_TASK, False)
    task_change(WATCHDOG_TASK, False, allow_missing=True)

    try:
        current_app = Path(str(previous["appExe"])) if previous else fallback_app
        stop_app([current_app] if current_app else [])
        baseline = max_pool_decision_id(db)
        journal["baselinePoolDecisionId"] = baseline
        snapshot = snapshot_before_handover(data_dir, manifest)
        journal["snapshotDir"] = str(snapshot)
        journal["phase"] = "snapshotted"
        atomic_json(data_dir / JOURNAL_FILE, journal)

        admin = str(manifest["poolAdminExe"])
        migration = run_json([admin, "migrate", "--db", str(db)], timeout=600)
        if migration.get("afterSchemaVersion") != EXPECTED_SCHEMA:
            raise ReleaseError("live migration did not reach schema 63")
        run_json([admin, "stamp-rights", "--db", str(db)], timeout=600)
        certification = certify_live(data_dir, manifest)
        queues = prove_canonical_queues(data_dir, manifest)
        if max_pool_decision_id(db) != baseline:
            raise ReleaseError("review decision history changed while the maintenance gate was active")
        atomic_json(data_dir / POINTER_FILE, manifest)
        journal["phase"] = "candidate-active"
        atomic_json(data_dir / JOURNAL_FILE, journal)

        launch_app(Path(str(manifest["appExe"])))
        wait_for_server(8737)
        prove_links(data_dir, manifest, funnel=False)
        prove_links(data_dir, manifest, funnel=True)
        register_release_tasks(manifest)
        task_change(WATCHDOG_TASK, True)
        task_change(LEGACY_WATCHDOG_TASK, False, allow_missing=True)
        if max_pool_decision_id(db) != baseline:
            raise ReleaseError("review decision history changed before candidate exposure")
        (data_dir / MAINTENANCE_FILE).unlink(missing_ok=True)
        journal["phase"] = "exposed"
        atomic_json(data_dir / JOURNAL_FILE, journal)

        supervision = Path(str(manifest["directory"])) / "scripts" / "check_supervision_live.py"
        run([sys.executable, str(supervision)], timeout=180)
        prove_links(data_dir, manifest, funnel=True)
        certify_live(data_dir, manifest)
        (data_dir / JOURNAL_FILE).unlink(missing_ok=True)
        unregister_task(RECOVERY_TASK)
        print(
            f"PRIVATE_PRODUCTION_RELEASE=READY release={manifest['releaseId']} schema=63 "
            f"reviewers={','.join(queues)} reviewReady={certification['gates']['reviewReady']}"
        )
        return 0
    except BaseException as error:
        print(f"RELEASE HANDOVER FAILED: {error}", file=sys.stderr)
        try:
            recover(data_dir, release_root)
        except BaseException as recovery_error:
            print(f"AUTOMATIC RECOVERY BLOCKED: {recovery_error}", file=sys.stderr)
        raise


def defaults() -> tuple[Path, Path]:
    appdata = os.environ.get("APPDATA")
    localappdata = os.environ.get("LOCALAPPDATA")
    if not appdata or not localappdata:
        raise ReleaseError("APPDATA and LOCALAPPDATA are required; pass explicit directories")
    return Path(appdata) / "cortex-speech", Path(localappdata) / "CortexSpeech" / "private-production-releases"


def parser() -> argparse.ArgumentParser:
    default_data, default_releases = defaults()
    root = Path(__file__).resolve().parent.parent
    value = argparse.ArgumentParser(description=__doc__)
    commands = value.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("stage", help="stage and clone-prove a candidate without touching live state")
    deploy_parser = commands.add_parser("deploy", help="perform the protected live handover")
    for target in (stage, deploy_parser):
        target.add_argument("--candidate-dir", type=Path, required=True)
        target.add_argument("--source-root", type=Path, default=root)
        target.add_argument("--git-sha", required=True)
        target.add_argument("--data-dir", type=Path, default=default_data)
        target.add_argument("--release-root", type=Path, default=default_releases)
    deploy_parser.add_argument("--fallback-app", type=Path)
    deploy_parser.add_argument("--fallback-watchdog", type=Path)
    recover_parser = commands.add_parser("recover", help="resume the fail-closed recovery journal")
    recover_parser.add_argument("--data-dir", type=Path, default=default_data)
    recover_parser.add_argument("--release-root", type=Path, default=default_releases)
    return value


def main() -> int:
    args = parser().parse_args()
    if args.command == "recover":
        recover(args.data_dir.resolve(strict=True), args.release_root.resolve())
        return 0
    args.stage_only = args.command == "stage"
    return deploy(args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:  # noqa: BLE001 - release automation must fail closed on every unexpected fault
        print(f"PRIVATE PRODUCTION RELEASE: FAIL - {error}", file=sys.stderr)
        raise SystemExit(1)
