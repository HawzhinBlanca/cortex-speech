#!/usr/bin/env python3
"""Versioned, fail-closed Cortex private-production release handover.

The controller stages immutable binaries before downtime, migrates only behind a maintenance marker,
proves the live reviewer links without minting sessions, and exposes the candidate only after every
read-only production gate passes. A persistent journal plus a scheduled recovery arm makes an
interrupted handover recoverable after a process crash or reboot.
"""

from __future__ import annotations

import argparse
import contextlib
import ctypes
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

POINTER_FILE = "active-private-production-release.json"
JOURNAL_FILE = "pending-private-production-release.json"
MAINTENANCE_FILE = "private-production-maintenance.json"
RELEASE_MANIFEST_FILE = "release-manifest.json"
DEDUP_MANIFEST_FILE = "review-pool-dedup-manifest.json"
SCHEMA_CONTRACT_FILE = "private_production_schema_contract.v1.json"
SCHEMA_CONTRACT_RELATIVE_PATH = f"scripts/{SCHEMA_CONTRACT_FILE}"
LEGACY_WATCHDOG_TASK = "CortexWatchdog"
WATCHDOG_TASK = "CortexPrivateProductionWatchdog"
RESTORE_TASK = "CortexDailyRestoreDrill"
RECOVERY_TASK = "CortexReleaseRecovery"
# Handle-based (auto-released on process death, so power loss can never leave it stale) mutex
# between a live deploy and the scheduled recovery arm. Before this existed the arm's first fire at
# T+2min ran recover() CONCURRENTLY with any deploy slower than two minutes — rolling back a
# healthy in-flight handover mid-migration (2026-08-30 audit; that day's deploy escaped by ~50s).
HANDOVER_LOCK_FILE = "release-handover.lock"
# Written on every failed recovery attempt and cleared on success, so the alarm forwarder can page
# the owner when the arm is failing — previously a persistently failing recovery burned its whole
# repetition window in silence and left every couch route 503 forever.
RECOVERY_FAILURE_FILE = "release-recovery-failure.json"
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA64 = re.compile(r"^[0-9a-f]{64}$")
BINARY_SHA_MARKER = re.compile(rb"CORTEX_BUILD_SHA:([0-9a-f]{40}|unknown)(?![0-9a-f])")
LEGACY_V1_MANIFEST_FIELDS = {
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
    "dedupManifest",
    "dedupManifestSha256",
}
MANIFEST_FIELDS = LEGACY_V1_MANIFEST_FIELDS | {
    "schemaContract",
    "schemaContractId",
    "schemaContractSha256",
}
SCHEMA_CONTRACT_FIELDS = {
    "schema",
    "contractId",
    "targetSchema",
    "supportedMigrationSources",
    "sameSchemaRecovery",
    "normalization",
    "algorithm",
    "migrationSource",
    "migrationSourceSha256",
    "historicalPrefixThroughSchema",
    "historicalPrefixSha256",
    "appendOnlyContract",
    "appendOnlyContractSha256",
}
SCHEMA_CONTRACT_ID = "cortex-private-production-schema-65-to-70-v1"
SCHEMA_CONTRACT_TARGET = 70
# Migration sources this controller has proven on a live-sized clone: the schema-65 legacy boundary
# (v1 pointer) and the schema-69 line that served until the dedup-supersession release.
SCHEMA_CONTRACT_SOURCES = [65, 69]
# Contracts a COMPATIBLE PREVIOUS release (schema-2 pointer) may still carry: id -> (target, sources).
# A 69 pointer is the last-known-good during a 69->70 handover and is validated against its own
# contract, never against the current one.
PREVIOUS_SCHEMA_CONTRACTS = {"cortex-private-production-schema-65-to-69-v1": (69, [65])}
PRODUCTION_SCHEMA_BOUNDARY = 65
HISTORICAL_PREFIX_START = "pub static MIGRATIONS: &[Migration] = &["
FIRST_POST_PRODUCTION_MIGRATION = "    Migration {\n        version: 66,"
JOURNAL_FIELDS = {
    "schema",
    "phase",
    "startedAtUtc",
    "sourceSchema",
    "baselinePoolDecisionId",
    "candidate",
    "previousActive",
    "fallbackApp",
    "fallbackWatchdog",
    "snapshotDir",
    "snapshotManifestSha256",
    "targetDatabaseSha256",
}
JOURNAL_PHASES = {
    "prepared",
    "maintenance",
    "snapshotted",
    "candidate-certified",
    "candidate-active",
    "exposed",
}
PROFILE_STATE = (
    "settings.json",
    "champion.json",
    "reviewer_dialects.json",
    "voice_focus.json",
    "review_pilot_policy.json",
)


from policy_python import sha256_file


class ReleaseError(RuntimeError):
    pass


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


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
        durable_replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def durable_replace(source: Path, destination: Path) -> None:
    """Atomically replace one same-directory file and make the rename metadata durable."""

    if os.name == "nt":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.MoveFileExW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
        kernel32.MoveFileExW.restype = ctypes.c_int
        movefile_replace_existing = 0x00000001
        movefile_write_through = 0x00000008
        if not kernel32.MoveFileExW(
            str(source),
            str(destination),
            movefile_replace_existing | movefile_write_through,
        ):
            error = ctypes.get_last_error()
            raise OSError(error, f"durable replacement failed for {destination}")
        return

    os.replace(source, destination)
    descriptor = os.open(destination.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


@contextlib.contextmanager
def exclusive_instance_lock(data_dir: Path):
    """Hold the same no-sharing authority used by every Windows Cortex writer."""

    lock_path = data_dir / "cortex.lock"
    if os.name == "nt":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.argtypes = [
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
        ]
        kernel32.CreateFileW.restype = ctypes.c_void_p
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel32.CloseHandle.restype = ctypes.c_int
        generic_read = 0x80000000
        generic_write = 0x40000000
        open_always = 4
        file_attribute_normal = 0x80
        invalid_handle = ctypes.c_void_p(-1).value
        handle = kernel32.CreateFileW(
            str(lock_path),
            generic_read | generic_write,
            0,
            None,
            open_always,
            file_attribute_normal,
            None,
        )
        if handle == invalid_handle:
            error = ctypes.get_last_error()
            raise ReleaseError(
                f"database replacement refused because another Cortex writer holds {lock_path} "
                f"(Windows error {error})"
            )
        try:
            yield
        finally:
            if not kernel32.CloseHandle(handle):
                raise OSError(ctypes.get_last_error(), f"failed to release {lock_path}")
            try:
                lock_path.unlink(missing_ok=True)
            except PermissionError:
                # A new legitimate owner may have acquired the file after our handle closed.
                pass
        return

    import fcntl

    handle = lock_path.open("a+b")
    try:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise ReleaseError(f"database replacement refused because another Cortex writer holds {lock_path}") from error
        yield
    finally:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
        finally:
            handle.close()
        # Keep the stable inode. Unlinking after unlock creates a race where a new owner can lock
        # this inode just before it is removed and a third process then locks a replacement inode.


def try_acquire_handover_lock(data_dir: Path):
    """Exclusive handover mutex, or None if a live deploy/recovery process already holds it.

    Handle-based on both platforms: the OS releases it the instant the holder dies, so a power
    loss mid-deploy can never strand a stale lock that blocks recovery (the failure class that
    existence-based lockfiles carry). Windows uses share-mode-0 CreateFileW; POSIX uses flock.
    """
    lock_path = data_dir / HANDOVER_LOCK_FILE
    if os.name == "nt":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.restype = ctypes.c_void_p
        handle = kernel32.CreateFileW(
            ctypes.c_wchar_p(str(lock_path)),
            ctypes.c_uint32(0x80000000 | 0x40000000),  # GENERIC_READ | GENERIC_WRITE
            ctypes.c_uint32(0),  # no sharing: the second opener fails while the first lives
            None,
            ctypes.c_uint32(4),  # OPEN_ALWAYS
            ctypes.c_uint32(0x80),  # FILE_ATTRIBUTE_NORMAL
            None,
        )
        if handle == ctypes.c_void_p(-1).value:
            return None
        return ("nt", kernel32, handle)
    import fcntl

    posix_handle = lock_path.open("a+b")
    try:
        fcntl.flock(posix_handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        posix_handle.close()
        return None
    return ("posix", fcntl, posix_handle)


def release_handover_lock(lock) -> None:
    if lock is None:
        return
    kind, module, handle = lock
    if kind == "nt":
        module.CloseHandle(ctypes.c_void_p(handle))
        return
    try:
        module.flock(handle.fileno(), module.LOCK_UN)
    finally:
        handle.close()


def record_recovery_failure(data_dir: Path, error: BaseException) -> None:
    """Best-effort breadcrumb for the alarm forwarder; never masks the real recovery error."""
    try:
        log_dir = data_dir / "logs"
        log_dir.mkdir(parents=True, exist_ok=True)
        atomic_json(
            log_dir / RECOVERY_FAILURE_FILE,
            {"failedAtUtc": utc_now(), "error": str(error)[:2000]},
        )
    except BaseException:
        pass


def clear_recovery_failure(data_dir: Path) -> None:
    try:
        (data_dir / "logs" / RECOVERY_FAILURE_FILE).unlink(missing_ok=True)
    except BaseException:
        pass


def is_within(child: Path, parent: Path) -> bool:
    try:
        child.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def validate_artifact(path: Path, label: str) -> Path:
    if path.is_symlink():
        raise ReleaseError(f"{label} must not be a symlink: {path}")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise ReleaseError(f"{label} is missing or inaccessible: {path}: {error}") from error
    if not resolved.is_file():
        raise ReleaseError(f"{label} must be a regular non-symlink file: {resolved}")
    if resolved.stat().st_size <= 0:
        raise ReleaseError(f"{label} is empty: {resolved}")
    return resolved


def validate_baked_git_sha(path: Path, expected_git_sha: str, label: str) -> str:
    """Require one exact compile-time Git marker in every shipped Rust executable."""

    resolved = validate_artifact(path, label)
    if not SHA40.fullmatch(expected_git_sha):
        raise ReleaseError(f"{label} expected Git SHA is invalid")
    actual: str | None = None
    carry = b""
    try:
        with resolved.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                window = carry + chunk
                safe_start_limit = max(0, len(window) - 80)
                for match in BINARY_SHA_MARKER.finditer(window):
                    if match.start() >= safe_start_limit:
                        break
                    if actual is not None:
                        raise ReleaseError(f"{label} must contain exactly one CORTEX_BUILD_SHA marker")
                    actual = match.group(1).decode("ascii")
                # The longest marker is 57 bytes. Keep enough overlap to discover a marker whose
                # prefix starts at the end of one block without buffering a multi-hundred-MB exe.
                carry = window[-80:]
            for match in BINARY_SHA_MARKER.finditer(carry):
                if actual is not None:
                    raise ReleaseError(f"{label} must contain exactly one CORTEX_BUILD_SHA marker")
                actual = match.group(1).decode("ascii")
    except OSError as error:
        raise ReleaseError(f"{label} build identity cannot be read: {error}") from error
    if actual is None:
        raise ReleaseError(f"{label} must contain exactly one CORTEX_BUILD_SHA marker")
    if actual != expected_git_sha:
        raise ReleaseError(
            f"{label} was built from Git SHA {actual}, not declared release SHA {expected_git_sha}"
        )
    return actual


def _normalized_lf_bytes(path: Path, label: str) -> bytes:
    resolved = validate_artifact(path, label)
    try:
        text = resolved.read_bytes().decode("utf-8")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{label} is not valid UTF-8") from error
    normalized = text.replace("\r\n", "\n")
    if "\r" in normalized:
        raise ReleaseError(f"{label} contains unsupported bare carriage returns")
    return normalized.encode("utf-8")


def validate_schema_contract(
    path: Path,
    *,
    expected_id: str = SCHEMA_CONTRACT_ID,
    expected_target: int = SCHEMA_CONTRACT_TARGET,
    expected_sources: list[int] | None = None,
) -> tuple[Path, dict[str, Any], str]:
    """Validate the one release/migration authority and every source identity it pins.

    Defaults pin the CURRENT contract. A compatible previous release passes its own (older)
    expectations so its pointer stays a valid last-known-good during a handover.
    """

    if expected_sources is None:
        expected_sources = SCHEMA_CONTRACT_SOURCES
    resolved = validate_artifact(path, "private-production schema contract")
    value = load_json(resolved)
    if set(value) != SCHEMA_CONTRACT_FIELDS:
        raise ReleaseError(
            "schema contract fields are invalid "
            f"(missing={sorted(SCHEMA_CONTRACT_FIELDS - set(value))}, "
            f"extra={sorted(set(value) - SCHEMA_CONTRACT_FIELDS)})"
        )
    if type(value["schema"]) is not int or value["schema"] != 1:
        raise ReleaseError("schema contract schema must be integer 1")
    if value["contractId"] != expected_id:
        raise ReleaseError(f"schema contract identity is not the approved {expected_id} authority")
    if type(value["targetSchema"]) is not int or value["targetSchema"] != expected_target:
        raise ReleaseError(f"schema contract target must be exactly {expected_target}")
    sources = value["supportedMigrationSources"]
    if sources != expected_sources or any(type(item) is not int for item in sources):
        raise ReleaseError(
            f"schema contract must support exactly the proven migration sources {expected_sources}"
        )
    if value["sameSchemaRecovery"] is not True:
        raise ReleaseError(f"schema contract must explicitly permit same-schema {expected_target} recovery")
    if value["normalization"] != "utf8-lf" or value["algorithm"] != "sha256":
        raise ReleaseError("schema contract hash algorithm/normalization is unsupported")
    if value["migrationSource"] != "src-tauri/src/migrations/mod.rs":
        raise ReleaseError("schema contract migration source path is not canonical")
    if value["appendOnlyContract"] != "scripts/append_only_migration_contract.v1.json":
        raise ReleaseError("schema contract append-only authority path is not canonical")
    if value["historicalPrefixThroughSchema"] != PRODUCTION_SCHEMA_BOUNDARY:
        raise ReleaseError("schema contract historical production boundary is not schema 65")
    for field in ("migrationSourceSha256", "historicalPrefixSha256", "appendOnlyContractSha256"):
        if not isinstance(value[field], str) or not SHA64.fullmatch(value[field]):
            raise ReleaseError(f"schema contract {field} is invalid")

    # The contract always lives at <release-root>/scripts/<name>. Derive all bound paths from that
    # root, require their canonical relative names, and reject symlink/path substitution.
    source_root = resolved.parent.parent.resolve(strict=True)
    migration = validate_artifact(source_root / str(value["migrationSource"]), "canonical migration source")
    append_only = validate_artifact(
        source_root / str(value["appendOnlyContract"]), "append-only migration contract"
    )
    if not is_within(migration, source_root) or not is_within(append_only, source_root):
        raise ReleaseError("schema contract source authority escapes its release root")
    migration_bytes = _normalized_lf_bytes(migration, "canonical migration source")
    migration_sha = hashlib.sha256(migration_bytes).hexdigest()
    if migration_sha != value["migrationSourceSha256"]:
        raise ReleaseError("canonical migration source does not match the schema contract")
    if sha256_file(append_only) != value["appendOnlyContractSha256"]:
        raise ReleaseError("append-only migration contract does not match the schema contract")

    migration_text = migration_bytes.decode("utf-8")
    try:
        prefix_start = migration_text.index(HISTORICAL_PREFIX_START)
        prefix_end = migration_text.index(FIRST_POST_PRODUCTION_MIGRATION, prefix_start)
    except ValueError as error:
        raise ReleaseError("canonical schema-65 migration boundary is missing") from error
    prefix_sha = hashlib.sha256(migration_text[prefix_start:prefix_end].encode("utf-8")).hexdigest()
    if prefix_sha != value["historicalPrefixSha256"]:
        raise ReleaseError("historical migrations 1-65 do not match the production authority")

    catalog_end = migration_text.find("\n];", prefix_start)
    if catalog_end < 0:
        raise ReleaseError("canonical migration catalog terminator is missing")
    versions = [
        int(item)
        for item in re.findall(
            r"Migration\s*\{\s*version:\s*([0-9]+)\s*,",
            migration_text[prefix_start:catalog_end],
        )
    ]
    target = int(value["targetSchema"])
    if versions != list(range(1, target + 1)):
        raise ReleaseError(f"migration catalog is not the exact contiguous schema 1..={target} authority")
    return resolved, value, sha256_file(resolved)


_BUILTIN_SCHEMA_CONTRACT_PATH = Path(__file__).resolve().with_name(SCHEMA_CONTRACT_FILE)
_, _BUILTIN_SCHEMA_CONTRACT, _BUILTIN_SCHEMA_CONTRACT_SHA256 = validate_schema_contract(
    _BUILTIN_SCHEMA_CONTRACT_PATH
)
EXPECTED_SCHEMA = int(_BUILTIN_SCHEMA_CONTRACT["targetSchema"])
SUPPORTED_MIGRATION_SOURCES = frozenset(int(value) for value in _BUILTIN_SCHEMA_CONTRACT["supportedMigrationSources"])


def validate_dedup_manifest(path: Path) -> tuple[Path, str]:
    resolved = validate_artifact(path, "review-pool dedup manifest")
    value = load_json(resolved)
    claimed = value.get("manifestSha256")
    summary = value.get("summary")
    if value.get("manifestSchema") != 1 or not isinstance(claimed, str) or not SHA64.fullmatch(claimed):
        raise ReleaseError("review-pool dedup manifest identity is invalid")
    if not isinstance(summary, dict) or summary.get("unconfirmedRiskGroups") != 0:
        raise ReleaseError("review-pool dedup manifest has unresolved risk")
    payload = dict(value)
    payload.pop("manifestSha256", None)
    actual = hashlib.sha256(
        json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    if actual != claimed:
        raise ReleaseError("review-pool dedup manifest payload does not match its digest")
    return resolved, claimed


def operations_bundle_sha256(root: Path, *, allow_legacy_missing_dialect: bool = False) -> str:
    """Bind staged operations, optionally recognizing the prior release digest shape."""
    files = [
        path
        for path in (root / "scripts").rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.suffix.lower() != ".pyc"
    ]
    migrations = root / "src-tauri" / "src" / "migrations" / "mod.rs"
    dialects = root / "src-tauri" / "src" / "dialect.rs"
    if (
        not migrations.is_file()
        or not files
        or (not dialects.is_file() and not allow_legacy_missing_dialect)
    ):
        raise ReleaseError(
            "operations bundle is missing scripts, the canonical migration ledger, or dialect authority"
        )
    files.append(migrations)
    if dialects.is_file():
        files.append(dialects)
    digest = hashlib.sha256()
    for path in sorted(files, key=lambda value: value.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content_sha = sha256_file(path).encode("ascii")
        digest.update(len(relative).to_bytes(4, "big"))
        digest.update(relative)
        digest.update(path.stat().st_size.to_bytes(8, "big"))
        digest.update(content_sha)
    return digest.hexdigest()


def validate_manifest(
    value: dict[str, Any], *, expected_root: Path | None = None, allow_compatible_previous: bool = False
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReleaseError("release manifest must be one JSON object")
    fields = set(value)
    current = fields == MANIFEST_FIELDS
    legacy_v1 = allow_compatible_previous and fields == LEGACY_V1_MANIFEST_FIELDS
    if not current and not legacy_v1:
        raise ReleaseError(
            f"release manifest fields are invalid (missing={sorted(MANIFEST_FIELDS - set(value))}, "
            f"extra={sorted(set(value) - MANIFEST_FIELDS)})"
        )
    expected_manifest_schema = 2 if current else 1
    if type(value["schema"]) is not int or value["schema"] != expected_manifest_schema:
        raise ReleaseError(f"release manifest schema must be integer {expected_manifest_schema}")
    expected_database_schema = EXPECTED_SCHEMA if current else PRODUCTION_SCHEMA_BOUNDARY
    declared_schema = value["expectedDatabaseSchema"]
    # A compatible previous release may sit on a proven migration source below the current
    # target (the schema-69 line during the 69->70 handover). Only when the caller asked for
    # previous-compatibility, only for a versioned schema-2 pointer, never for the legacy boundary.
    previous_source = (
        current
        and allow_compatible_previous
        and type(declared_schema) is int
        and declared_schema in SUPPORTED_MIGRATION_SOURCES
        and declared_schema != PRODUCTION_SCHEMA_BOUNDARY
        and declared_schema != EXPECTED_SCHEMA
    )
    if not previous_source and (type(declared_schema) is not int or declared_schema != expected_database_schema):
        raise ReleaseError(f"release manifest must require database schema {expected_database_schema}")
    if not isinstance(value["appGitSha"], str) or not SHA40.fullmatch(value["appGitSha"]):
        raise ReleaseError("release manifest appGitSha is invalid")
    directory = Path(str(value["directory"])).resolve(strict=True)
    if expected_root is not None and not is_within(directory, expected_root):
        raise ReleaseError("release directory escapes the configured immutable release root")
    artifacts: dict[str, Path] = {}
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
        artifacts[path_field] = artifact
    validate_baked_git_sha(artifacts["appExe"], value["appGitSha"], "release app")
    validate_baked_git_sha(artifacts["poolAdminExe"], value["appGitSha"], "release pool_admin")
    operations_sha = value["operationsSha256"]
    if not isinstance(operations_sha, str) or not SHA64.fullmatch(operations_sha):
        raise ReleaseError("operationsSha256 is invalid")
    if (
        operations_bundle_sha256(directory, allow_legacy_missing_dialect=allow_compatible_previous)
        != operations_sha
    ):
        raise ReleaseError("the staged operations bundle does not match its release SHA-256")
    dedup_path = Path(str(value["dedupManifest"]))
    if not is_within(dedup_path, directory):
        raise ReleaseError("dedupManifest escapes the immutable release directory")
    _, dedup_sha = validate_dedup_manifest(dedup_path)
    if dedup_sha != value["dedupManifestSha256"]:
        raise ReleaseError("the staged dedup manifest does not match its release digest")
    if current:
        expected_contract_path = (directory / SCHEMA_CONTRACT_RELATIVE_PATH).resolve(strict=True)
        contract_path = Path(str(value["schemaContract"])).resolve(strict=True)
        if contract_path != expected_contract_path:
            raise ReleaseError("schemaContract is not the canonical contract inside the immutable release")
        if previous_source:
            previous = PREVIOUS_SCHEMA_CONTRACTS.get(str(value["schemaContractId"]))
            if previous is None or previous[0] != declared_schema:
                raise ReleaseError("previous release schema contract is not a proven migration-source authority")
            _, contract, contract_sha = validate_schema_contract(
                contract_path,
                expected_id=str(value["schemaContractId"]),
                expected_target=previous[0],
                expected_sources=previous[1],
            )
        else:
            _, contract, contract_sha = validate_schema_contract(contract_path)
        if value["schemaContractId"] != contract["contractId"]:
            raise ReleaseError("release schema contract identity does not match its staged authority")
        claimed_contract_sha = value["schemaContractSha256"]
        if not isinstance(claimed_contract_sha, str) or not SHA64.fullmatch(claimed_contract_sha):
            raise ReleaseError("schemaContractSha256 is invalid")
        if claimed_contract_sha != contract_sha:
            raise ReleaseError("staged schema contract does not match its release SHA-256")
    return value


def copy_source_bundle(source_root: Path, stage: Path) -> None:
    scripts = source_root / "scripts"
    migrations = source_root / "src-tauri" / "src" / "migrations" / "mod.rs"
    dialects = source_root / "src-tauri" / "src" / "dialect.rs"
    if not scripts.is_dir() or not migrations.is_file() or not dialects.is_file():
        raise ReleaseError(
            "source root is missing scripts, the canonical migration ledger, or dialect authority"
        )
    shutil.copytree(
        scripts,
        stage / "scripts",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    migration_target = stage / "src-tauri" / "src" / "migrations"
    migration_target.mkdir(parents=True)
    shutil.copy2(migrations, migration_target / "mod.rs")
    shutil.copy2(dialects, migration_target.parent / "dialect.rs")


def stage_release(
    candidate_dir: Path,
    source_root: Path,
    release_root: Path,
    git_sha: str,
    dedup_manifest: Path | None = None,
) -> dict[str, Any]:
    if not SHA40.fullmatch(git_sha):
        raise ReleaseError("--git-sha must be the exact lowercase 40-character release commit")
    candidate_dir = candidate_dir.resolve(strict=True)
    source_root = source_root.resolve(strict=True)
    release_root = release_root.resolve() if release_root.exists() else release_root.absolute()
    if is_within(candidate_dir, release_root):
        raise ReleaseError("candidate build must be outside the live immutable release root")
    app_source = validate_artifact(candidate_dir / "cortex-speech-app.exe", "candidate app")
    admin_source = validate_artifact(candidate_dir / "pool_admin.exe", "candidate pool_admin")
    validate_baked_git_sha(app_source, git_sha, "candidate app")
    validate_baked_git_sha(admin_source, git_sha, "candidate pool_admin")
    dedup_source, dedup_sha = validate_dedup_manifest(dedup_manifest or source_root / DEDUP_MANIFEST_FILE)
    _, schema_contract, schema_contract_sha = validate_schema_contract(
        source_root / SCHEMA_CONTRACT_RELATIVE_PATH
    )
    app_sha = sha256_file(app_source)
    admin_sha = sha256_file(admin_source)
    operations_sha = operations_bundle_sha256(source_root)
    release_id = (
        f"{git_sha[:12]}-{app_sha[:12]}-{operations_sha[:12]}-"
        f"{schema_contract_sha[:12]}-{dedup_sha[:12]}"
    )
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
        shutil.copy2(dedup_source, stage / DEDUP_MANIFEST_FILE)
        copy_source_bundle(source_root, stage)
        if operations_bundle_sha256(stage) != operations_sha:
            raise ReleaseError("staged operations bundle changed while it was copied")
        _, staged_contract, staged_contract_sha = validate_schema_contract(
            stage / SCHEMA_CONTRACT_RELATIVE_PATH
        )
        if staged_contract != schema_contract or staged_contract_sha != schema_contract_sha:
            raise ReleaseError("staged schema contract changed while it was copied")
        watchdog = stage / "scripts" / "ops" / "cortex-watchdog.ps1"
        validate_artifact(watchdog, "watchdog script")
        manifest = {
            "schema": 2,
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
            "schemaContract": str(final / SCHEMA_CONTRACT_RELATIVE_PATH),
            "schemaContractId": schema_contract["contractId"],
            "schemaContractSha256": schema_contract_sha,
            "dedupManifest": str(final / DEDUP_MANIFEST_FILE),
            "dedupManifestSha256": dedup_sha,
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
            rows = connection.execute("SELECT version FROM schema_migrations ORDER BY version").fetchall()
            versions = [row[0] for row in rows]
            if not versions or any(type(version) is not int for version in versions):
                raise ReleaseError("database has no authoritative schema migration history")
            if versions != list(range(1, versions[-1] + 1)):
                raise ReleaseError("database migration history is not one contiguous authority")
            return int(versions[-1])
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


def database_content_sha256(db_path: Path) -> str:
    """Hash one read-only logical snapshot, including WAL-visible committed rows.

    A raw main-file hash can miss committed WAL frames and therefore cannot authorize rollback.
    ``iterdump`` walks SQLite's transactionally visible schema and rows; length framing makes the
    comparison unambiguous. The digest is used only within one interrupted handover, never as a
    cross-version schema fingerprint.
    """

    digest = hashlib.sha256()
    try:
        connection = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True, timeout=30)
        try:
            connection.execute("PRAGMA query_only=ON")
            for statement in connection.iterdump():
                encoded = statement.encode("utf-8")
                digest.update(len(encoded).to_bytes(8, "big"))
                digest.update(encoded)
        finally:
            connection.close()
    except (sqlite3.Error, UnicodeError) as error:
        raise ReleaseError(f"database content authority cannot be hashed: {error}") from error
    return digest.hexdigest()


def preflight_clone(data_dir: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    db_path = data_dir / "cortex-speech.db"
    with tempfile.TemporaryDirectory(prefix="cortex-release-preflight-") as raw:
        clone = Path(raw)
        sqlite_backup(db_path, clone / "cortex-speech.db")
        source_schema = database_schema(clone / "cortex-speech.db")
        allowed = SUPPORTED_MIGRATION_SOURCES | {EXPECTED_SCHEMA}
        if source_schema not in allowed:
            raise ReleaseError(
                f"clone preflight accepts only schema {sorted(SUPPORTED_MIGRATION_SOURCES)}->schema {EXPECTED_SCHEMA} "
                f"or same-schema {EXPECTED_SCHEMA}, not schema {source_schema}"
            )
        for name in PROFILE_STATE:
            source = data_dir / name
            if source.is_file():
                shutil.copy2(source, clone / name)
        admin = str(manifest["poolAdminExe"])
        migration = run_json([admin, "migrate", "--db", str(clone / "cortex-speech.db")], timeout=300)
        if migration.get("appGitSha") != manifest["appGitSha"]:
            raise ReleaseError("candidate migration came from a different release commit")
        if migration.get("beforeSchemaVersion") != source_schema:
            raise ReleaseError("candidate migration did not report the clone's exact source schema")
        if migration.get("afterSchemaVersion") != EXPECTED_SCHEMA:
            raise ReleaseError(f"candidate migration did not reach schema {EXPECTED_SCHEMA}")
        expected_migrated = source_schema != EXPECTED_SCHEMA
        if migration.get("migrated") is not expected_migrated:
            raise ReleaseError("candidate migration changed-state report contradicts the schema boundary")
        run_json(
            [admin, "apply-dedup", "--db", str(clone / "cortex-speech.db"), "--manifest", str(manifest["dedupManifest"])],
            timeout=600,
        )
        rights = run_json([admin, "stamp-rights", "--db", str(clone / "cortex-speech.db")], timeout=300)
        report = run_json([admin, "certify", "--db", str(clone / "cortex-speech.db"), "--full-integrity"], timeout=600)
        if report.get("appGitSha") != manifest["appGitSha"]:
            raise ReleaseError("candidate pool_admin is not built from the declared release commit")
        if report.get("databaseSchemaVersion") != EXPECTED_SCHEMA:
            raise ReleaseError(f"candidate did not migrate the live-sized clone to schema {EXPECTED_SCHEMA}")
        if report.get("database", {}).get("healthy") is not True:
            raise ReleaseError("candidate clone database certification failed")
        if report.get("audio", {}).get("allAvailable") is not True:
            raise ReleaseError("candidate clone has missing or changed pool audio")
        if report.get("rights", {}).get("allExact") is not True:
            raise ReleaseError("candidate clone did not establish exact owner rights")
        return {
            "sourceSchemaVersion": source_schema,
            "migration": migration,
            "rights": rights,
            "certification": report,
        }


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
function Resolve-ImagePath([string]$path) {
    # Get-Process reports the image path exactly as the process was launched, so an app started
    # through an 8.3 component (C:\PROGRA~1\..., or a hosted runner's shortened profile dir) never
    # string-equals the long path this controller resolved. Left un-normalised the filter matches
    # nothing, stop_app returns success without stopping anything, and the release goes on to
    # overwrite files and launch a second instance while the old one still holds the database.
    # Normalise the kernel path against the filesystem so both sides are the same identity; the
    # comparison stays an exact whole-path match, so only the named executable is ever touched.
    $item = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
    if ($item) { return $item.FullName.ToLowerInvariant() }
    return $path.ToLowerInvariant()
}
function Get-TargetedProcesses {
    @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and ($targets -contains (Resolve-ImagePath $_.Path))
    })
}
$processes = @(Get-TargetedProcesses)
foreach ($process in $processes) { [void]$process.CloseMainWindow() }
if ($processes.Count) { Wait-Process -Id $processes.Id -Timeout $env:CORTEX_RELEASE_STOP_TIMEOUT -ErrorAction SilentlyContinue }
$left = @(Get-TargetedProcesses)
foreach ($process in $left) { Stop-Process -Id $process.Id -Force }
if ($left.Count) { Wait-Process -Id $left.Id -Timeout 10 -ErrorAction SilentlyContinue }
# A process that TerminateProcess has already accepted can stay in the kernel's process list while
# its threads are torn down. Measured 2026-09-02 on a hosted Windows runner: a force-stopped stand-in
# was still enumerable after the 10-second wait, and the single check that used to sit here aborted
# with "did not stop" although nothing had survived. Re-check for a bounded time; whatever is still
# listed at the end is a genuine survivor and fails the stop exactly as before, now with its identity.
$deadline = (Get-Date).AddSeconds(15)
$survivors = @(Get-TargetedProcesses)
while ($survivors.Count -and ((Get-Date) -lt $deadline)) {
    Start-Sleep -Milliseconds 250
    $survivors = @(Get-TargetedProcesses)
}
if ($survivors.Count) {
    $detail = ($survivors | ForEach-Object {
        $exited = try { $_.HasExited } catch { "?" }
        "pid=$($_.Id) exited=$exited path=$($_.Path)"
    }) -join "; "
    throw "Cortex app process did not stop after the force deadline: $detail"
}
"""
    env["CORTEX_RELEASE_STOP_TIMEOUT"] = str(force_after_seconds)
    # The script waits up to force_after_seconds, then 10s after the force stop, then re-checks for up to
    # 15s more, so the wrapper's own bound sits well above that sum.
    run(["powershell.exe", "-NoProfile", "-Command", script], timeout=force_after_seconds + 60, env=env)


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
            raise ReleaseError(f"canonical release queue is empty for reviewer {reviewer}")
        if (
            probe.get("passes") is not True
            or probe.get("sampleAudioValidWav") is not True
            or probe.get("submissionIdempotencyAuthority") is not True
        ):
            raise ReleaseError(f"canonical release audio/idempotency probe failed for reviewer {reviewer}")
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
            raise ReleaseError(f"canonical release queue latency failed for reviewer {reviewer}")
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
    manifest_schema = int(manifest["expectedDatabaseSchema"])
    if report.get("databaseSchemaVersion") != manifest_schema:
        raise ReleaseError(f"live certification did not prove schema {manifest_schema}")
    if report.get("rights", {}).get("allExact") is not True:
        raise ReleaseError("live pool rights are incomplete or conflicting")
    if report.get("audio", {}).get("allAvailable") is not True:
        raise ReleaseError("live pool audio is incomplete or changed")
    return report


def validate_snapshot_manifest_authority(
    snapshot: Path,
    *,
    expected_sha256: str | None = None,
) -> str:
    """Verify the complete sealed snapshot inventory and bind its exact manifest bytes."""

    if snapshot.is_symlink():
        raise ReleaseError(f"rollback snapshot must not be a symlink: {snapshot}")
    resolved = snapshot.resolve(strict=True)
    if not resolved.is_dir():
        raise ReleaseError(f"rollback snapshot is not a directory: {resolved}")
    manifest_path = validate_artifact(resolved / "SNAPSHOT_MANIFEST.json", "rollback snapshot manifest")
    digest = sha256_file(manifest_path)
    if expected_sha256 is not None:
        if not SHA64.fullmatch(expected_sha256):
            raise ReleaseError("rollback snapshot manifest authority is invalid")
        if digest != expected_sha256:
            raise ReleaseError("rollback snapshot is not the exact snapshot captured for this handover")
    try:
        from restore_drill import SnapshotValidationError, validate_snapshot_manifest

        validate_snapshot_manifest(resolved)
    except (ImportError, OSError, SnapshotValidationError, ValueError) as error:
        raise ReleaseError(f"rollback snapshot manifest validation failed: {error}") from error
    return digest


def snapshot_before_handover(data_dir: Path, manifest: dict[str, Any]) -> tuple[Path, str]:
    script = Path(str(manifest["directory"])) / "scripts" / "create_recovery_snapshot.py"
    live_database = data_dir / "cortex-speech.db"
    live_schema = database_schema(live_database)
    label = f"preprivate_v{live_schema}_to_v{EXPECTED_SCHEMA}"
    result = run([sys.executable, str(script), "--data-dir", str(data_dir), "--label", label], timeout=600)
    local = next((line.split("=", 1)[1].strip() for line in result.stdout.splitlines() if line.startswith("LOCAL_SNAPSHOT=")), None)
    if not local:
        raise ReleaseError("pre-handover snapshot command did not report a local snapshot")
    snapshot = Path(local).resolve(strict=True)
    if not (snapshot / "cortex-speech.db").is_file() or not (snapshot / "SNAPSHOT_MANIFEST.json").is_file():
        raise ReleaseError("pre-handover snapshot is incomplete")
    manifest_sha = validate_snapshot_manifest_authority(snapshot)
    snapshot_database = snapshot / "cortex-speech.db"
    if database_schema(snapshot_database) != live_schema:
        raise ReleaseError("pre-handover snapshot database schema differs from the stopped live database")
    if database_content_sha256(snapshot_database) != database_content_sha256(live_database):
        raise ReleaseError("pre-handover snapshot is not the exact stopped live database generation")
    # Bind the same closed inventory again after logical comparison. A concurrent replacement of
    # either the manifest or its listed database cannot win the validate/compare handoff.
    validate_snapshot_manifest_authority(snapshot, expected_sha256=manifest_sha)
    return snapshot, manifest_sha


def restore_database(
    snapshot: Path,
    data_dir: Path,
    expected_schema: int,
    expected_manifest_sha256: str,
) -> Path:
    with exclusive_instance_lock(data_dir):
        return _restore_database_locked(
            snapshot,
            data_dir,
            expected_schema,
            expected_manifest_sha256,
        )


def _restore_database_locked(
    snapshot: Path,
    data_dir: Path,
    expected_schema: int,
    expected_manifest_sha256: str,
) -> Path:
    validate_snapshot_manifest_authority(snapshot, expected_sha256=expected_manifest_sha256)
    source = validate_artifact(snapshot / "cortex-speech.db", "rollback snapshot database")
    source_digest = database_content_sha256(source)
    if database_schema(source) != expected_schema:
        raise ReleaseError("rollback snapshot schema does not match the pre-handover database")
    live = data_dir / "cortex-speech.db"
    temporary = data_dir / f".cortex-speech.rollback.{os.getpid()}.{time.time_ns()}.db"
    try:
        sqlite_backup(source, temporary)
        if database_schema(temporary) != expected_schema:
            raise ReleaseError("staged rollback database failed schema verification")
        if database_content_sha256(temporary) != source_digest:
            raise ReleaseError("staged rollback database is not the exact logical snapshot authority")
        check = sqlite3.connect(temporary)
        try:
            if check.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise ReleaseError("staged rollback database failed integrity_check")
            if check.execute("PRAGMA foreign_key_check").fetchone() is not None:
                raise ReleaseError("staged rollback database has foreign-key violations")
        finally:
            check.close()
        # Revalidate after the backup so a changed snapshot can never win a validate/copy race.
        validate_snapshot_manifest_authority(snapshot, expected_sha256=expected_manifest_sha256)

        quarantine = data_dir / "recovery-quarantine"
        quarantine.mkdir(exist_ok=True)
        live_schema = database_schema(live)
        live_digest = database_content_sha256(live)
        preserved = quarantine / f"cortex-speech.failed-v{live_schema}.{time.time_ns()}.db"
        preserved_temporary = quarantine / f".{preserved.name}.{os.getpid()}.tmp"
        try:
            sqlite_backup(live, preserved_temporary)
            if database_content_sha256(preserved_temporary) != live_digest:
                raise ReleaseError("failed database quarantine copy lost WAL-visible committed state")
            durable_replace(preserved_temporary, preserved)
        finally:
            preserved_temporary.unlink(missing_ok=True)

        # Checkpoint before detaching sidecars. If another writer changes the database anywhere in
        # this boundary, refuse the replacement and leave the current database authoritative.
        checkpoint = sqlite3.connect(live, timeout=30)
        try:
            mode = str(checkpoint.execute("PRAGMA journal_mode").fetchone()[0]).lower()
            if mode == "wal":
                busy, _log, _checkpointed = checkpoint.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
                if int(busy) != 0:
                    raise ReleaseError("live database WAL is busy; rollback refuses a concurrent writer")
        finally:
            checkpoint.close()
        if database_content_sha256(live) != live_digest:
            raise ReleaseError("live database changed while rollback was preparing its replacement")

        for suffix in ("-wal", "-shm"):
            sidecar = Path(str(live) + suffix)
            if sidecar.exists():
                # Keep raw forensic bytes without giving them SQLite's magic companion filename.
                # A logical backup has its own page salts; attaching the failed database's WAL/SHM
                # to it could replay foreign frames into the very quarantine copy meant to preserve
                # evidence, or make that copy appear corrupt on its next open.
                durable_replace(sidecar, Path(str(preserved) + f".source{suffix}"))
        durable_replace(temporary, live)
        return preserved
    finally:
        temporary.unlink(missing_ok=True)


def rollback_policy(
    source_schema: int,
    current_schema: int,
    baseline_id: int,
    current_id: int,
    previous_schema: int | None,
    *,
    database_changed: bool = False,
) -> str:
    if current_schema > EXPECTED_SCHEMA:
        return "blocked"
    if source_schema in SUPPORTED_MIGRATION_SOURCES and current_schema == source_schema:
        # No database rollback is needed or permitted here. A decision may have completed after the
        # first baseline read but before maintenance stopped the old app; resume the exact compatible
        # binary and preserve that forward progress.
        return "resume-pre-migration"
    if current_id > baseline_id or database_changed:
        if current_schema == EXPECTED_SCHEMA and previous_schema == EXPECTED_SCHEMA:
            return "binary-only"
        return "preserve-current" if current_schema == EXPECTED_SCHEMA else "blocked"
    if source_schema in SUPPORTED_MIGRATION_SOURCES and current_schema == EXPECTED_SCHEMA:
        return "restore-pre-migration"
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
    return validate_manifest(load_json(path), expected_root=release_root, allow_compatible_previous=True) if path.is_file() else None


def register_release_tasks(manifest: dict[str, Any]) -> None:
    root = Path(str(manifest["directory"]))
    powershell_file(
        root / "scripts" / "ops" / "cortex-watchdog.ps1",
        "-Register",
        "-TaskName",
        WATCHDOG_TASK,
    )
    powershell_file(root / "scripts" / "ops" / "cortex-daily-restore-drill.ps1", "-Register")


def validate_release_journal(
    path: Path,
    release_root: Path,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any] | None]:
    journal = load_json(path)
    if set(journal) != JOURNAL_FIELDS:
        raise ReleaseError(
            "release journal fields are invalid "
            f"(missing={sorted(JOURNAL_FIELDS - set(journal))}, extra={sorted(set(journal) - JOURNAL_FIELDS)})"
        )
    if type(journal["schema"]) is not int or journal["schema"] != 2:
        raise ReleaseError("release journal schema must be integer 2")
    if journal["phase"] not in JOURNAL_PHASES:
        raise ReleaseError("release journal phase is invalid")
    source_schema = journal["sourceSchema"]
    if type(source_schema) is not int or source_schema not in (SUPPORTED_MIGRATION_SOURCES | {EXPECTED_SCHEMA}):
        raise ReleaseError(
            f"release journal source must be a proven schema {sorted(SUPPORTED_MIGRATION_SOURCES)} or {EXPECTED_SCHEMA}"
        )
    baseline = journal["baselinePoolDecisionId"]
    if type(baseline) is not int or baseline < 0:
        raise ReleaseError("release journal decision baseline is invalid")
    candidate_value = journal["candidate"]
    if not isinstance(candidate_value, dict):
        raise ReleaseError("release journal candidate is invalid")
    candidate = validate_manifest(candidate_value, expected_root=release_root)
    previous_value = journal["previousActive"]
    previous: dict[str, Any] | None = None
    if previous_value is not None:
        if not isinstance(previous_value, dict):
            raise ReleaseError("release journal previousActive is invalid")
        previous = validate_manifest(previous_value, expected_root=release_root, allow_compatible_previous=True)
    if source_schema == PRODUCTION_SCHEMA_BOUNDARY:
        if previous is not None and int(previous["expectedDatabaseSchema"]) != PRODUCTION_SCHEMA_BOUNDARY:
            raise ReleaseError("schema-65 handover previous release is not a schema-65 legacy boundary")
    elif previous is None or int(previous["expectedDatabaseSchema"]) != source_schema:
        # Same-schema recovery and every versioned migration source alike: the last-known-good must
        # be the exact release that served the pre-handover database.
        raise ReleaseError(f"schema-{source_schema} handover requires a schema-{source_schema} previous release")

    digest = journal["targetDatabaseSha256"]
    if digest is not None and (not isinstance(digest, str) or not SHA64.fullmatch(digest)):
        raise ReleaseError("release journal target database digest is invalid")
    if journal["phase"] in {"candidate-certified", "candidate-active", "exposed"} and digest is None:
        raise ReleaseError("release journal lost the post-migration database authority")
    for field in ("fallbackApp", "fallbackWatchdog", "snapshotDir"):
        if journal[field] is not None and (not isinstance(journal[field], str) or not journal[field]):
            raise ReleaseError(f"release journal {field} is invalid")
    snapshot_digest = journal["snapshotManifestSha256"]
    if snapshot_digest is not None and (
        not isinstance(snapshot_digest, str) or not SHA64.fullmatch(snapshot_digest)
    ):
        raise ReleaseError("release journal snapshot manifest authority is invalid")
    if (journal["snapshotDir"] is None) != (snapshot_digest is None):
        raise ReleaseError("release journal snapshot directory and manifest authority must be paired")
    if source_schema != EXPECTED_SCHEMA and journal["phase"] in {
        "snapshotted",
        "candidate-certified",
        "candidate-active",
        "exposed",
    }:
        if journal["snapshotDir"] is None:
            raise ReleaseError(f"schema-{source_schema} handover journal lost its bound rollback snapshot")
    return journal, candidate, previous


def recover(data_dir: Path, release_root: Path) -> bool:
    journal_path = data_dir / JOURNAL_FILE
    if not journal_path.is_file():
        unregister_task(RECOVERY_TASK)
        clear_recovery_failure(data_dir)
        return True
    # A live deploy (or another recovery) holds the handover lock for its whole duration. Deferring
    # here is what stops the scheduled arm's T+2min fire from rolling back a healthy in-flight
    # handover; the arm simply tries again in five minutes, and a dead holder releases the
    # handle-based lock instantly.
    lock = try_acquire_handover_lock(data_dir)
    if lock is None:
        print("RELEASE RECOVERY: deferred — a live deploy or recovery process holds the handover lock")
        return True
    try:
        result = _recover_under_lock(data_dir, release_root, journal_path)
        clear_recovery_failure(data_dir)
        return result
    except BaseException as error:
        # The breadcrumb the alarm forwarder pages on: a failing recovery arm previously burned its
        # whole repetition window in silence, leaving every couch route 503 with no app running.
        record_recovery_failure(data_dir, error)
        raise
    finally:
        release_handover_lock(lock)


def _recover_under_lock(data_dir: Path, release_root: Path, journal_path: Path) -> bool:
    journal, candidate, previous = validate_release_journal(journal_path, release_root)
    source_schema = int(journal["sourceSchema"])
    baseline = int(journal["baselinePoolDecisionId"])
    db = data_dir / "cortex-speech.db"
    current_schema = database_schema(db)
    if current_schema > EXPECTED_SCHEMA:
        raise ReleaseError(
            f"automatic recovery refuses future database schema {current_schema}; "
            f"this release supports at most {EXPECTED_SCHEMA}"
        )
    if current_schema not in {source_schema, EXPECTED_SCHEMA}:
        raise ReleaseError(
            f"interrupted handover reached unsupported schema {current_schema} from source schema {source_schema}"
        )
    current_id = max_pool_decision_id(db)
    database_changed = False
    target_digest = journal["targetDatabaseSha256"]
    if source_schema in SUPPORTED_MIGRATION_SOURCES and current_schema == EXPECTED_SCHEMA:
        if target_digest is not None:
            database_changed = database_content_sha256(db) != target_digest
        elif journal["phase"] in {"candidate-certified", "candidate-active", "exposed"}:
            # validate_release_journal normally catches this. Keep the recovery decision locally
            # fail-closed even if a future journal reader relaxes shape validation.
            raise ReleaseError("post-migration database authority is missing; rollback safety is unknowable")
    mode = rollback_policy(
        source_schema,
        current_schema,
        baseline,
        current_id,
        int(previous["expectedDatabaseSchema"]) if previous else None,
        database_changed=database_changed,
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
            preserved = restore_database(
                snapshot,
                data_dir,
                source_schema,
                str(journal["snapshotManifestSha256"]),
            )
        if previous is not None and int(previous["expectedDatabaseSchema"]) == source_schema:
            atomic_json(data_dir / POINTER_FILE, previous)
            launch_app(Path(str(previous["appExe"])))
            wait_for_server(8737)
            certify_live(data_dir, previous)
            prove_links(data_dir, previous, funnel=False)
            prove_links(data_dir, previous, funnel=True)
            prove_canonical_queues(data_dir, previous)
            register_release_tasks(previous)
            task_change(WATCHDOG_TASK, True)
            task_change(LEGACY_WATCHDOG_TASK, False, allow_missing=True)
            (data_dir / MAINTENANCE_FILE).unlink(missing_ok=True)
            journal_path.unlink(missing_ok=True)
            unregister_task(RECOVERY_TASK)
            action = "resumed" if preserved is None else f"restored; failed schema-v{current_schema} database preserved at {preserved}"
            print(f"RELEASE RECOVERY: {action} managed schema-v{source_schema} release {previous['releaseId']}")
            return True
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
            print(f"RELEASE RECOVERY: restored schema v{source_schema}; failed schema-v{current_schema} database preserved at {preserved}")
        return True

    target = previous if mode == "binary-only" else candidate if mode == "preserve-current" else None
    if target is None:
        raise ReleaseError(
            "automatic rollback is blocked: restoring an older database could destroy reviewer work, "
            f"and no schema-{EXPECTED_SCHEMA}-compatible last-known-good release is available"
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
    print(
        f"RELEASE RECOVERY: activated schema-{EXPECTED_SCHEMA}-compatible release "
        f"{target['releaseId']} without restoring the database"
    )
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
    # Held for the whole deploy. The scheduled recovery arm defers while this process lives, and
    # the OS releases the handle the instant this process dies — so a crash hands recovery over
    # immediately, while a slow-but-healthy deploy can never be rolled back mid-flight again.
    handover_lock = try_acquire_handover_lock(data_dir)
    if handover_lock is None:
        raise ReleaseError("another deploy or recovery process holds the handover lock")
    source_schema = database_schema(db)
    if source_schema not in (SUPPORTED_MIGRATION_SOURCES | {EXPECTED_SCHEMA}):
        raise ReleaseError(
            f"deployment accepts only the proven v{sorted(SUPPORTED_MIGRATION_SOURCES)}->v{EXPECTED_SCHEMA} "
            f"or same-schema v{EXPECTED_SCHEMA} path, not schema v{source_schema}"
        )
    session_reviewers(data_dir)
    previous = active_pointer(data_dir, release_root)
    if previous is not None and previous.get("expectedDatabaseSchema") != source_schema:
        raise ReleaseError("the active release pointer is not compatible with the pre-migration database")
    if source_schema == EXPECTED_SCHEMA and previous is None:
        raise ReleaseError(
            f"a schema-{EXPECTED_SCHEMA} deployment requires a versioned last-known-good active release"
        )
    manifest = stage_release(args.candidate_dir, args.source_root, release_root, args.git_sha, args.dedup_manifest)
    print(f"STAGED_RELEASE={manifest['releaseId']}")
    preflight = preflight_clone(data_dir, manifest)
    if preflight["sourceSchemaVersion"] != source_schema:
        raise ReleaseError("clone preflight source schema differs from the live database boundary")
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
        raise ReleaseError(
            f"the first managed pre-v{EXPECTED_SCHEMA} deployment requires --fallback-app and --fallback-watchdog"
        )

    baseline = max_pool_decision_id(db)
    journal: dict[str, Any] = {
        "schema": 2,
        "phase": "prepared",
        "startedAtUtc": utc_now(),
        "sourceSchema": source_schema,
        "baselinePoolDecisionId": baseline,
        "candidate": manifest,
        "previousActive": previous,
        "fallbackApp": str(fallback_app) if fallback_app else None,
        "fallbackWatchdog": str(fallback_watchdog) if fallback_watchdog else None,
        "snapshotDir": None,
        "snapshotManifestSha256": None,
        "targetDatabaseSha256": None,
    }
    atomic_json(data_dir / JOURNAL_FILE, journal)
    # The recovery arm is registered AFTER the journal exists but BEFORE the maintenance marker is
    # written. The old order (marker first, arm second) left a seconds-wide hard-kill window (power
    # loss during Register-ScheduledTask) in which reviewers were 503-blocked by the marker while
    # NO recovery task existed and the still-enabled watchdog faithfully kept the 503-serving old
    # app alive forever. This order closes it: every possible fire of the arm sees the journal, and
    # any fire while this process lives defers on the handover lock it holds.
    recovery = Path(str(manifest["directory"])) / "scripts" / "ops" / "cortex-release-recovery.ps1"
    powershell_file(recovery, "-Register")
    write_maintenance(data_dir, str(manifest["releaseId"]))
    journal["phase"] = "maintenance"
    atomic_json(data_dir / JOURNAL_FILE, journal)
    task_change(LEGACY_WATCHDOG_TASK, False)
    task_change(WATCHDOG_TASK, False, allow_missing=True)

    try:
        current_app = Path(str(previous["appExe"])) if previous else fallback_app
        stop_app([current_app] if current_app else [])
        baseline = max_pool_decision_id(db)
        journal["baselinePoolDecisionId"] = baseline
        # Persist the last writer-free baseline before starting snapshot I/O. A crash in this
        # interval must resume against the exact decision frontier observed after process stop,
        # never the earlier pre-maintenance observation.
        atomic_json(data_dir / JOURNAL_FILE, journal)
        snapshot, snapshot_manifest_sha = snapshot_before_handover(data_dir, manifest)
        journal["snapshotDir"] = str(snapshot)
        journal["snapshotManifestSha256"] = snapshot_manifest_sha
        journal["phase"] = "snapshotted"
        atomic_json(data_dir / JOURNAL_FILE, journal)

        admin = str(manifest["poolAdminExe"])
        migration = run_json([admin, "migrate", "--db", str(db)], timeout=600)
        if migration.get("appGitSha") != manifest["appGitSha"]:
            raise ReleaseError("live migration came from a different release commit")
        if migration.get("beforeSchemaVersion") != source_schema:
            raise ReleaseError("live migration did not report the exact source schema")
        if migration.get("afterSchemaVersion") != EXPECTED_SCHEMA:
            raise ReleaseError(f"live migration did not reach schema {EXPECTED_SCHEMA}")
        expected_migrated = source_schema != EXPECTED_SCHEMA
        if migration.get("migrated") is not expected_migrated:
            raise ReleaseError("live migration changed-state report contradicts the schema boundary")
        run_json(
            [admin, "apply-dedup", "--db", str(db), "--manifest", str(manifest["dedupManifest"])],
            timeout=600,
        )
        run_json([admin, "stamp-rights", "--db", str(db)], timeout=600)
        certification = certify_live(data_dir, manifest)
        queues = prove_canonical_queues(data_dir, manifest)
        if max_pool_decision_id(db) != baseline:
            raise ReleaseError("review decision history changed while the maintenance gate was active")
        journal["targetDatabaseSha256"] = database_content_sha256(db)
        journal["phase"] = "candidate-certified"
        atomic_json(data_dir / JOURNAL_FILE, journal)
        if database_content_sha256(db) != journal["targetDatabaseSha256"]:
            raise ReleaseError("database changed after candidate certification and before activation")
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
        if database_content_sha256(db) != journal["targetDatabaseSha256"]:
            raise ReleaseError("database changed after candidate activation and before exposure")
        (data_dir / MAINTENANCE_FILE).unlink(missing_ok=True)
        journal["phase"] = "exposed"
        atomic_json(data_dir / JOURNAL_FILE, journal)

        supervision = Path(str(manifest["directory"])) / "scripts" / "check_supervision_live.py"
        run([sys.executable, str(supervision)], timeout=180)
        prove_links(data_dir, manifest, funnel=True)
        certify_live(data_dir, manifest)
        (data_dir / JOURNAL_FILE).unlink(missing_ok=True)
        unregister_task(RECOVERY_TASK)
        clear_recovery_failure(data_dir)
        release_handover_lock(handover_lock)
        print(
            f"PRIVATE_PRODUCTION_RELEASE=READY release={manifest['releaseId']} schema={EXPECTED_SCHEMA} "
            f"reviewers={','.join(queues)} reviewReady={certification['gates']['reviewReady']}"
        )
        return 0
    except BaseException as error:
        print(f"RELEASE HANDOVER FAILED: {error}", file=sys.stderr)
        # The inline recover needs the lock this process still holds; hand it over first.
        release_handover_lock(handover_lock)
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
        target.add_argument("--dedup-manifest", type=Path, required=True)
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
