#!/usr/bin/env python3
"""Prepare hash-bound read-only owner-product proof inputs and fresh disposable attempts.

The bundle is an evidence boundary, not a convenience copier. Every input is a pre-declared
SHA-256 authority. Sources are opened read-only, checked for path indirection and SQLite sidecars,
copied into a new staging tree, and rehashed before atomic publication. The canonical manifest
contains logical roles and relative paths only; source paths are never persisted.

Only the hash-bound schema-60 scale derivative is migrated. Migration is delegated to the narrow
``owner_proof_db`` Rust helper so it traverses the application's real migration and schema-contract
path. The campaign-exact authority is inspected through a detached snapshot and remains byte-exact.
There is no cleanup/delete/sanitize command for authorities or campaign policy.
"""

from __future__ import annotations

import ctypes
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import uuid
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Mapping, Protocol

from owner_proof_build import (
    GitAuthority,
    run_contained as _run_contained,
    run_link_preflight as _run_native_link_preflight,
)
from owner_proof_contract import (
    DATABASE_ROLES,
    FULL_GIT_SHA,
    HELPER_BUILD_TOOLCHAIN_FIELDS,
    SOURCE_ROLES,
    SourcePaths,
    canonical_json_bytes,
    canonical_sha256 as _canonical_sha256,
    expect_keys as _expect_keys,
    load_json as _load_json,
    lower_sha256 as _lower_sha256,
    positive_int as _positive_int,
    relative_path as _relative_path,
    validate_contract,
)
from owner_proof_helper import SubprocessHelper
import owner_proof_validation as _owner_proof_validation
from owner_proof_database import (
    ascii_lower as _ascii_lower,
    normalized_schema_sql as _normalized_schema_sql,
    reject_sqlite_sidecars as _reject_sqlite_sidecars,
    schema_fingerprint as _schema_fingerprint,
    sidecar as _sidecar,
    table_exists as _table_exists,
    validate_inspection as _validate_inspection,
)
from owner_proof_database import inspect_sqlite_readonly as _inspect_sqlite_readonly
from owner_proof_platform import (
    FILE_ATTRIBUTE_REPARSE_POINT,
    LockedFile,
    LockedToolchainTrees,
    NamedMutex,
    OwnedDirectoryLock as _OwnedDirectoryLock,
    ProofInputError,
    WindowsBuildTools,
    access_is_denied as _access_is_denied,
    delete_owned_tree_windows as _platform_delete_owned_tree_windows,
    fsync_directory as _platform_fsync_directory,
    owned_directory_identity as _owned_directory_identity,
    publish_sealed_directory as _publish_sealed_directory,
    remove_owned_staging as _platform_remove_owned_staging,
    stable_file_sha256 as _platform_stable_file_sha256,
    toolchain_tree_sha256,
)
from owner_proof_transaction import (
    PUBLISHED_TRANSACTION_FILES,
    begin_transaction as _begin_transaction,
    deterministic_staging_name as _deterministic_staging_name,
    record_seal_plan as _record_seal_plan,
    recover_staging_transaction as _recover_staging_transaction,
    validate_published_transaction as _validate_published_transaction,
)
APP_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = APP_ROOT.parent
DEFAULT_CONTRACT = Path(__file__).with_name("owner_proof_input_contract.v1.json")
RELEASE_CONTRACT_SHA256 = "5e3154af1e42b3c26a8eaba5d9b5ad5ab89c50d4d116fa6be72e26c0e6b22451"
MANIFEST_NAME = "manifest.v1.json"
CONTRACT_BUNDLE_PATH = "contract.v1.json"
HELPER_BUNDLE_PATH = "tools/owner_proof_db.exe"
HELPER_SOURCE_BUNDLE_PATH = "tools/owner_proof_db.rs"
HELPER_REPO_PATH = "cortex-speech-app/src-tauri/src/bin/owner_proof_db.rs"
CARGO_CONFIG_REPO_PATH = "cortex-speech-app/src-tauri/.cargo/config.toml"
ATTEMPTS_DIR = "attempts"
BUNDLE_DIR = "bundle.v1"
VERIFY_ROOT_DIR = "verify-work"
STAGING_PREFIX = ".owner-proof-inputs.staging-"
ATTEMPT_STAGING_PREFIX = ".attempt-"
VERIFY_BUILD_PREFIX = ".owner-proof-helper-verify-"
VERIFY_LEASE_NAME = "lease.v1.json"
REPRODUCIBLE_BUILD_PROTOCOL = "rustc-remap-source-date-epoch-msvc-brepro-v1"
FILE_ATTRIBUTE_READONLY = 0x1
class Helper(Protocol):
    git_sha: str
    helper_sha256: str
    helper_source_sha256: str

    def schema_contract(self, *, expected_schema: int) -> dict[str, Any]: ...

    def inspect(self, database: Path, *, expected_schema: int, campaign: str) -> dict[str, Any]: ...

    def migrate(
        self,
        source_database: Path,
        output_database: Path,
        *,
        staging_root: Path,
        source_sha256: str,
        expected_source_schema: int,
        expected_target_schema: int,
    ) -> dict[str, Any]: ...
HelperFactory = Callable[[Path, str, str, str], Helper]


def load_contract(path: Path, *, expected_sha256: str = RELEASE_CONTRACT_SHA256) -> dict[str, Any]:
    _assert_safe_existing_file(path, role="contract", reject_protected=False, reject_snapshot=False)
    contract = validate_contract(_load_json(path))
    if _canonical_sha256(contract) != expected_sha256:
        raise ProofInputError("proof-input contract is not the exact release authority")
    return contract


def _absolute_lexical(path: Path) -> Path:
    raw = os.fspath(path)
    if not raw or "\x00" in raw:
        raise ProofInputError("proof-input paths cannot be empty or contain NUL")
    absolute = Path(os.path.abspath(raw))
    if any(part == ".." for part in path.parts):
        raise ProofInputError("parent traversal is not permitted in proof-input paths")
    return absolute


def _metadata_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & FILE_ATTRIBUTE_REPARSE_POINT)


def _assert_no_links(path: Path, *, allow_missing_leaf: bool = False) -> Path:
    absolute = _absolute_lexical(path)
    chain = list(absolute.parents)[::-1] + [absolute]
    for index, item in enumerate(chain):
        try:
            metadata = os.lstat(item)
        except FileNotFoundError:
            if allow_missing_leaf and index == len(chain) - 1:
                continue
            raise ProofInputError("proof-input path does not exist") from None
        except OSError as error:
            raise ProofInputError("proof-input path metadata cannot be read") from error
        if stat.S_ISLNK(metadata.st_mode) or _metadata_reparse(metadata):
            raise ProofInputError("proof-input path contains a symlink or reparse point")
    return absolute


def _strip_windows_verbatim_prefix(value: str) -> str:
    normalized = value.replace("/", "\\")
    folded = normalized.casefold()
    if folded.startswith("\\\\?\\unc\\"):
        return "\\\\" + normalized[8:]
    if folded.startswith("\\\\?\\") or folded.startswith("\\??\\"):
        return normalized[4:]
    return normalized


def _normalized_path(path: Path) -> str:
    """Return one comparison identity for long, verbatim, UNC, and 8.3 spellings."""
    absolute = _absolute_lexical(path)
    try:
        # On Windows, realpath resolves junctions and expands 8.3 names. ``strict=False`` also
        # resolves the deepest existing ancestor of a not-yet-created output path.
        resolved = os.path.realpath(os.fspath(absolute), strict=False)
    except (OSError, ValueError) as error:
        raise ProofInputError("proof-input path cannot be normalized safely") from error
    if os.name == "nt":
        resolved = _strip_windows_verbatim_prefix(resolved)
    return os.path.normcase(os.path.normpath(resolved))


def _is_within(path: Path, root: Path) -> bool:
    candidate = _normalized_path(path)
    parent = _normalized_path(root)
    try:
        return os.path.commonpath([candidate, parent]) == parent
    except ValueError:
        return False


def _windows_known_folder(identifier: str) -> Path:
    """Resolve a Windows Known Folder without trusting caller-controlled environment variables."""
    if os.name != "nt":
        raise ProofInputError("Windows Known Folder authority is unavailable on this platform")

    class Guid(ctypes.Structure):
        _fields_ = [
            ("data1", ctypes.c_uint32),
            ("data2", ctypes.c_uint16),
            ("data3", ctypes.c_uint16),
            ("data4", ctypes.c_ubyte * 8),
        ]

    parsed = uuid.UUID(identifier)
    node = parsed.node.to_bytes(6, "big")
    data4 = (ctypes.c_ubyte * 8)(parsed.clock_seq_hi_variant, parsed.clock_seq_low, *node)
    guid = Guid(parsed.time_low, parsed.time_mid, parsed.time_hi_version, data4)
    output = ctypes.c_void_p()
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
    ole32 = ctypes.WinDLL("ole32", use_last_error=True)
    shell32.SHGetKnownFolderPath.argtypes = [
        ctypes.POINTER(Guid),
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    shell32.SHGetKnownFolderPath.restype = ctypes.c_long
    ole32.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    ole32.CoTaskMemFree.restype = None
    result = shell32.SHGetKnownFolderPath(ctypes.byref(guid), 0, None, ctypes.byref(output))
    if result < 0 or not output.value:
        if output.value:
            ole32.CoTaskMemFree(output)
        raise ProofInputError("Windows Known Folder authority cannot be resolved")
    try:
        value = ctypes.wstring_at(output.value)
    finally:
        ole32.CoTaskMemFree(output)
    if not value:
        raise ProofInputError("Windows Known Folder authority resolved to an empty path")
    return Path(value)


def protected_roots() -> tuple[Path, ...]:
    if os.name == "nt":
        roaming = _windows_known_folder("3eb685db-65f9-4cf6-a03a-e3ef65729f3d")
        local = _windows_known_folder("f1b32785-6fba-4fcf-9d55-7b8e7f157091")
    else:
        # The product is Windows-only. This branch keeps synthetic policy tests usable elsewhere,
        # while a production CLI run remains barred by the contract's Windows target.
        roaming_raw = os.environ.get("APPDATA", "").strip()
        local_raw = os.environ.get("LOCALAPPDATA", "").strip()
        if not roaming_raw or not local_raw:
            raise ProofInputError("Windows AppData authority cannot be resolved")
        roaming, local = Path(roaming_raw), Path(local_raw)
    return (
        roaming / "cortex-speech",
        local / "CortexSpeech" / "private-production-releases",
    )


def _is_snapshot_path(path: Path) -> bool:
    return any(
        part.casefold() in {"snapshots", "pinned"} or part.casefold().startswith("snapshot_")
        for part in _absolute_lexical(path).parts
    )


def _reject_protected(path: Path) -> None:
    if any(_is_within(path, root) for root in protected_roots()):
        raise ProofInputError("live AppData and active release paths cannot be proof inputs or outputs")


def _assert_safe_existing_file(
    path: Path,
    *,
    role: str,
    reject_protected: bool = True,
    reject_snapshot: bool = True,
    require_single_link: bool = True,
) -> Path:
    absolute = _assert_no_links(path)
    if reject_protected:
        _reject_protected(absolute)
    if reject_snapshot and _is_snapshot_path(absolute):
        raise ProofInputError(f"{role} cannot come from snapshot recovery authority")
    try:
        metadata = os.lstat(absolute)
    except OSError as error:
        raise ProofInputError(f"{role} metadata cannot be read") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ProofInputError(f"{role} must be one regular file")
    if require_single_link and getattr(metadata, "st_nlink", 1) != 1:
        raise ProofInputError(f"{role} must have exactly one filesystem name")
    return absolute


def _assert_safe_output_root(path: Path, *, allow_existing: bool = False) -> Path:
    absolute = _absolute_lexical(path)
    if absolute.exists() and not allow_existing:
        raise ProofInputError("output root already exists; preexisting bundles are never overwritten")
    if os.path.lexists(absolute) and not absolute.exists():
        raise ProofInputError("output root is an indirect or inaccessible filesystem object")
    parent = _assert_no_links(absolute.parent)
    if not parent.is_dir():
        raise ProofInputError("output parent must be an existing directory")
    _reject_protected(absolute)
    if _is_within(absolute, REPO_ROOT):
        raise ProofInputError("proof bundles must be published outside the Git worktree")
    if _is_snapshot_path(absolute):
        raise ProofInputError("snapshot recovery trees cannot contain writable proof attempts")
    return absolute


def _state(path: Path, *, require_single_link: bool = True) -> tuple[int, int, int, int, int, int]:
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or _metadata_reparse(metadata) or not stat.S_ISREG(metadata.st_mode):
        raise ProofInputError("proof-input authority ceased to be a regular direct file")
    if require_single_link and getattr(metadata, "st_nlink", 1) != 1:
        raise ProofInputError("proof-input authority has an external hardlink alias")
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_mode,
        getattr(metadata, "st_file_attributes", 0),
    )


def _open_readonly_stable(
    path: Path, *, require_single_link: bool = True
) -> tuple[int, tuple[int, int, int, int, int, int]]:
    before = _state(path, require_single_link=require_single_link)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ProofInputError("proof-input authority cannot be opened read-only") from error
    opened = os.fstat(descriptor)
    opened_identity = (opened.st_dev, opened.st_ino, opened.st_size)
    if opened_identity != before[:3]:
        os.close(descriptor)
        raise ProofInputError("proof-input authority changed identity while opening")
    return descriptor, before


def _hash_stable_file(path: Path, *, require_single_link: bool = True) -> tuple[str, int, int]:
    descriptor, before = _open_readonly_stable(path, require_single_link=require_single_link)
    digest = hashlib.sha256()
    try:
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            digest.update(block)
        opened_after = os.fstat(descriptor)
    except OSError as error:
        raise ProofInputError("proof-input authority could not be hashed") from error
    finally:
        os.close(descriptor)
    after = _state(path, require_single_link=require_single_link)
    if after != before or (opened_after.st_dev, opened_after.st_ino, opened_after.st_size) != before[:3]:
        raise ProofInputError("proof-input authority changed while hashing")
    return digest.hexdigest(), before[2], before[4]


def _require_binary_git_marker(path: Path, git_sha: str, *, require_single_link: bool = True) -> None:
    marker = f"CORTEX_BUILD_SHA:{git_sha}".encode("ascii")
    descriptor, before = _open_readonly_stable(path, require_single_link=require_single_link)
    overlap = b""
    found = False
    try:
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            combined = overlap + block
            if marker in combined:
                found = True
            overlap = combined[-(len(marker) - 1) :]
    except OSError as error:
        raise ProofInputError("migration helper build marker cannot be inspected") from error
    finally:
        os.close(descriptor)
    if _state(path, require_single_link=require_single_link) != before:
        raise ProofInputError("migration helper changed while its build marker was inspected")
    if not found:
        raise ProofInputError("migration helper does not contain the exact release Git marker")


def _fsync_directory(path: Path) -> None:
    _platform_fsync_directory(path)


def _fsync_attempts_directory(path: Path) -> None:
    _platform_fsync_directory(path, desired_access=0x4)


def _publish_directory_no_overwrite(staging: Path, destination: Path) -> None:
    """Publish the exact locked directory object, never a re-resolved pathname replacement."""
    with _OwnedDirectoryLock(staging, publish=True) as locked:
        locked.publish_no_replace(destination, _fsync_directory)


def _publish_file_without_overwrite(temporary: Path, destination: Path) -> None:
    """Atomically create a destination name without a check/replace overwrite window."""
    original_mode = stat.S_IMODE(os.stat(temporary, follow_symlinks=False).st_mode)
    was_readonly = _is_readonly(temporary)
    try:
        os.link(temporary, destination, follow_symlinks=False)
    except FileExistsError as error:
        raise ProofInputError("preexisting proof file cannot be overwritten") from error
    except OSError as error:
        raise ProofInputError("proof file cannot be published with no-overwrite authority") from error
    try:
        if os.name == "nt" and was_readonly:
            _make_writable(temporary)
        temporary.unlink()
        if os.name == "nt" and was_readonly:
            os.chmod(destination, original_mode)
    except OSError as error:
        # Both names still identify the same bytes. Fail closed and leave the temporary name rather
        # than deleting either a destination another process may already be consuming.
        raise ProofInputError("published proof file retains an unexpected temporary hardlink") from error
    # File-byte fsync does not make the hard-link create/unlink namespace update
    # durable.  The nested destination directory must be flushed before a parent
    # publication can claim that this final name is durable.
    _fsync_directory(destination.parent)


def _atomic_write(path: Path, payload: bytes, *, readonly: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{uuid.uuid4().hex[:16]}")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0), 0o600)
    try:
        with os.fdopen(descriptor, "wb", closefd=True) as target:
            target.write(payload)
            target.flush()
            os.fsync(target.fileno())
        _publish_file_without_overwrite(temporary, path)
        if readonly:
            _make_readonly(path)
        _fsync_directory(path.parent)
    except Exception:
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass
        raise


def _copy_exact(
    source: Path,
    destination: Path,
    *,
    expected_sha256: str | None,
    source_require_single_link: bool = True,
) -> dict[str, Any]:
    before_hash, before_size, source_mode = _hash_stable_file(
        source, require_single_link=source_require_single_link
    )
    if expected_sha256 is not None and before_hash != expected_sha256:
        raise ProofInputError("proof-input source does not match its declared SHA-256 authority")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise ProofInputError("preexisting proof file cannot be overwritten")
    temporary = destination.with_name(f".{destination.name}.tmp-{uuid.uuid4().hex[:16]}")
    source_descriptor, source_state = _open_readonly_stable(
        source, require_single_link=source_require_single_link
    )
    target_descriptor = -1
    copied_digest = hashlib.sha256()
    try:
        target_descriptor = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0),
            0o600,
        )
        while True:
            block = os.read(source_descriptor, 1024 * 1024)
            if not block:
                break
            copied_digest.update(block)
            view = memoryview(block)
            while view:
                written = os.write(target_descriptor, view)
                if written <= 0:
                    raise ProofInputError("proof-input copy made no forward progress")
                view = view[written:]
        os.fsync(target_descriptor)
    except OSError as error:
        raise ProofInputError("proof-input copy failed") from error
    finally:
        os.close(source_descriptor)
        if target_descriptor >= 0:
            os.close(target_descriptor)
    if _state(source, require_single_link=source_require_single_link) != source_state:
        temporary.unlink(missing_ok=True)
        raise ProofInputError("proof-input source changed during copy")
    copied_hash = copied_digest.hexdigest()
    after_hash, after_size, _mode = _hash_stable_file(
        source, require_single_link=source_require_single_link
    )
    if before_hash != copied_hash or after_hash != before_hash or after_size != before_size:
        temporary.unlink(missing_ok=True)
        raise ProofInputError("proof-input source/copy hashes are not stable and identical")
    os.chmod(temporary, stat.S_IMODE(source_mode))
    _publish_file_without_overwrite(temporary, destination)
    destination_hash, destination_size, _mode = _hash_stable_file(destination)
    if destination_hash != before_hash or destination_size != before_size:
        raise ProofInputError("published proof-input copy does not match its source")
    _fsync_directory(destination.parent)
    return {"sha256": destination_hash, "sizeBytes": destination_size}


def _make_readonly(path: Path) -> None:
    mode = stat.S_IMODE(os.stat(path, follow_symlinks=False).st_mode)
    os.chmod(path, mode & ~(stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH))


def _make_writable(path: Path) -> None:
    mode = stat.S_IMODE(os.stat(path, follow_symlinks=False).st_mode)
    os.chmod(path, mode | stat.S_IWUSR)


def _is_readonly(path: Path) -> bool:
    metadata = os.stat(path, follow_symlinks=False)
    if os.name == "nt":
        return bool(getattr(metadata, "st_file_attributes", 0) & FILE_ATTRIBUTE_READONLY)
    return not bool(stat.S_IMODE(metadata.st_mode) & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH))


def inspect_sqlite_readonly(path: Path) -> dict[str, Any]:
    return _inspect_sqlite_readonly(
        path,
        assert_safe_existing_file=_assert_safe_existing_file,
        hash_stable_file=_hash_stable_file,
        absolute_lexical=_absolute_lexical,
    )


def _default_helper_factory(path: Path, helper_sha256: str, git_sha: str, helper_source_sha256: str) -> Helper:
    return SubprocessHelper(path, helper_sha256, git_sha, helper_source_sha256, _minimal_helper_environment)


def _git_authority(git_binary: Path | None) -> GitAuthority:
    return GitAuthority(git_binary, REPO_ROOT, _minimal_windows_build_environment)


def _git_environment(git_binary: Path | None) -> dict[str, str] | None:
    return _git_authority(git_binary).environment()


def _git_sha_clean(git_binary: Path | None = None, *, repository: Path = REPO_ROOT) -> str:
    return _git_authority(git_binary).clean_sha(repository)


def _git_tree_for_commit(git_sha: str, git_binary: Path | None = None) -> str:
    return _git_authority(git_binary).tree(git_sha)


def _git_commit_timestamp(git_sha: str, git_binary: Path | None = None) -> str:
    return _git_authority(git_binary).commit_timestamp(git_sha)


def _git_blob_sha256(git_sha: str, repo_relative_path: str, git_binary: Path | None = None) -> str:
    _relative_path(repo_relative_path, context="Git blob path")
    return _git_authority(git_binary).blob_sha256(git_sha, repo_relative_path)


def _materialize_release_source(build_root: Path, release_sha: str, git: Path) -> Path:
    return _git_authority(git).materialize(build_root, release_sha, _assert_no_links)


def _minimal_windows_build_environment() -> dict[str, str]:
    if os.name != "nt":
        return {}
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetWindowsDirectoryW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32]
    kernel32.GetWindowsDirectoryW.restype = ctypes.c_uint32
    buffer = ctypes.create_unicode_buffer(32768)
    length = kernel32.GetWindowsDirectoryW(buffer, len(buffer))
    if length == 0 or length >= len(buffer):
        raise ProofInputError("authoritative Windows directory cannot be resolved")
    return {"SystemRoot": buffer.value, "WINDIR": buffer.value}


def _minimal_helper_environment() -> dict[str, str]:
    environment = _minimal_windows_build_environment()
    if os.name == "nt":
        temporary = _assert_no_links(
            _windows_known_folder("f1b32785-6fba-4fcf-9d55-7b8e7f157091") / "Temp"
        )
        if not temporary.is_dir():
            raise ProofInputError("authoritative helper temporary directory is unavailable")
        environment["TEMP"] = os.fspath(temporary)
        environment["TMP"] = os.fspath(temporary)
    return environment


def _pinned_git_tool(toolchain: Mapping[str, Any]) -> Path:
    program_files = _windows_known_folder("905e63b6-c1bf-494e-b29c-65b732d3d21a")
    git = _assert_safe_existing_file(
        program_files / "Git" / "mingw64" / "bin" / "git.exe",
        role="pinned Git executable",
        reject_protected=False,
        reject_snapshot=True,
        require_single_link=False,
    )
    observed_hash, _size, _mode = _hash_stable_file(git, require_single_link=False)
    if observed_hash != toolchain["gitBinarySha256"]:
        raise ProofInputError("pinned Git binary hash differs from the release contract")
    try:
        result = _run_contained(
            [os.fspath(git), "--version"],
            cwd=REPO_ROOT,
            env=_minimal_windows_build_environment(),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="strict",
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError, UnicodeError) as error:
        raise ProofInputError("pinned Git identity cannot be read") from error
    if result.returncode != 0 or result.stdout.strip() != toolchain["gitVersion"]:
        raise ProofInputError("pinned Git version differs from the release contract")
    if toolchain_tree_sha256({"git-runtime": program_files / "Git"}) != toolchain["gitRuntimeTreeSha256"]:
        raise ProofInputError("pinned Git runtime tree differs from the release contract")
    return git


def _pinned_rust_tool(tool: str, toolchain: Mapping[str, Any]) -> Path:
    profile = _windows_known_folder("5e6c858f-0e22-4760-9afe-ea3317b67173")
    binary = _assert_safe_existing_file(
        profile / ".rustup" / "toolchains" / str(toolchain["channel"]) / "bin" / f"{tool}.exe",
        role=f"pinned {tool}",
        reject_protected=False,
        reject_snapshot=True,
        require_single_link=False,
    )
    expected_hash = str(toolchain[f"{tool}BinarySha256"])
    observed_hash, _size, _mode = _hash_stable_file(binary, require_single_link=False)
    if observed_hash != expected_hash:
        raise ProofInputError(f"pinned {tool} binary hash differs from the release contract")
    return binary


def _verify_rust_runtime(toolchain: Mapping[str, Any]) -> None:
    profile = _windows_known_folder("5e6c858f-0e22-4760-9afe-ea3317b67173")
    root = profile / ".rustup" / "toolchains" / str(toolchain["channel"])
    if toolchain_tree_sha256({"rust-bin": root / "bin", "rust-lib": root / "lib"}) != toolchain[
        "rustRuntimeTreeSha256"
    ]:
        raise ProofInputError("pinned Rust runtime tree differs from the release contract")


def _pinned_windows_build_tools(toolchain: Mapping[str, Any]) -> WindowsBuildTools:
    program_files_x86 = _windows_known_folder("7c5a40ef-a0fb-4bfc-874a-c0f2e0b9fa8e")
    msvc_root = program_files_x86 / "Microsoft Visual Studio" / "2022" / "BuildTools" / "VC" / "Tools" / "MSVC"
    msvc_root /= str(toolchain["msvcToolsVersion"])
    kits = program_files_x86 / "Windows Kits" / "10"
    sdk_version = str(toolchain["windowsSdkVersion"])
    roots = {
        "msvc": msvc_root,
        "sdk-bin": kits / "bin" / sdk_version,
        "sdk-include": kits / "Include" / sdk_version,
        "sdk-lib": kits / "Lib" / sdk_version,
    }
    for root in roots.values():
        direct = _assert_no_links(root)
        if not direct.is_dir():
            raise ProofInputError("pinned MSVC/Windows SDK closure is incomplete")
    msvc_bin = msvc_root / "bin" / "Hostx64" / "x64"
    sdk_bin = roots["sdk-bin"] / "x64"
    paths = {
        "clBinarySha256": msvc_bin / "cl.exe",
        "linkBinarySha256": msvc_bin / "link.exe",
        "libBinarySha256": msvc_bin / "lib.exe",
        "rcBinarySha256": sdk_bin / "rc.exe",
        "mtBinarySha256": sdk_bin / "mt.exe",
    }
    for field, path in paths.items():
        _assert_safe_existing_file(
            path,
            role=f"pinned {path.name}",
            reject_protected=False,
            reject_snapshot=True,
            require_single_link=False,
        )
        if _platform_stable_file_sha256(path) != toolchain[field]:
            raise ProofInputError(f"pinned {path.name} hash differs from the release contract")
    if toolchain_tree_sha256({"msvc": roots["msvc"]}) != toolchain["msvcTreeSha256"]:
        raise ProofInputError("pinned MSVC tree differs from the release contract")
    if toolchain_tree_sha256({key: roots[key] for key in ("sdk-bin", "sdk-include", "sdk-lib")}) != toolchain[
        "windowsSdkTreeSha256"
    ]:
        raise ProofInputError("pinned Windows SDK tree differs from the release contract")
    return WindowsBuildTools(
        msvc_root=msvc_root,
        sdk_include=roots["sdk-include"],
        sdk_lib=roots["sdk-lib"],
        msvc_bin=msvc_bin,
        sdk_bin=sdk_bin,
        cl=paths["clBinarySha256"],
        link=paths["linkBinarySha256"],
        lib=paths["libBinarySha256"],
        rc=paths["rcBinarySha256"],
        mt=paths["mtBinarySha256"],
    )


def _tool_commit(binary: Path, *, expected_commit: str) -> None:
    try:
        result = _run_contained(
            [os.fspath(binary), "--version", "--verbose"],
            cwd=REPO_ROOT,
            env=_minimal_windows_build_environment(),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="strict",
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError, UnicodeError) as error:
        raise ProofInputError("pinned Rust tool identity cannot be read") from error
    commits = [line.split(":", 1)[1].strip() for line in result.stdout.splitlines() if line.startswith("commit-hash:")]
    if result.returncode != 0 or commits != [expected_commit]:
        raise ProofInputError("pinned Rust tool commit differs from the release contract")


def _helper_toolchain_evidence(toolchain: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "toolchainChannel": toolchain["channel"],
        **{field: toolchain[field] for field in HELPER_BUILD_TOOLCHAIN_FIELDS},
    }


def _require_exact_cargo_configuration(
    *,
    cargo_home: Path,
    release_sha: str,
    git: Path,
    expected_sha256: str,
    project_root: Path | None = None,
) -> Path:
    """Bind Cargo to the one tracked project config and reject every alternate config authority."""
    project = APP_ROOT / "src-tauri" if project_root is None else project_root
    config = _assert_safe_existing_file(
        project / ".cargo" / "config.toml",
        role="owner-proof Cargo configuration",
        reject_protected=False,
        reject_snapshot=True,
    )
    observed, _size, _mode = _hash_stable_file(config)
    if observed != expected_sha256 or observed != _git_blob_sha256(release_sha, CARGO_CONFIG_REPO_PATH, git):
        raise ProofInputError("owner-proof Cargo configuration differs from the exact release contract")
    home = _assert_no_links(cargo_home)
    if not home.is_dir():
        raise ProofInputError("Cargo source home is not one direct directory")
    allowed = _normalized_path(config)
    bases = {project, *project.parents, home}
    for base in bases:
        directory = base / ".cargo" if base != home else base
        for name in ("config", "config.toml"):
            candidate = directory / name
            if os.path.lexists(candidate) and _normalized_path(candidate) != allowed:
                raise ProofInputError("an alternate Cargo configuration could influence the owner-proof helper build")
    return config


def _build_release_helper(
    staging: Path,
    release_sha: str,
    toolchain: Mapping[str, Any],
    git: Path,
) -> tuple[Path, LockedFile, str, int, Path, tuple[int, int], dict[str, Any]]:
    """Build from an exact detached commit with the complete pinned Windows link closure."""
    if _git_sha_clean(git) != release_sha:
        raise ProofInputError("release checkout changed before helper build")
    cargo = _pinned_rust_tool("cargo", toolchain)
    rustc = _pinned_rust_tool("rustc", toolchain)
    _tool_commit(cargo, expected_commit=str(toolchain["cargoCommitHash"]))
    _tool_commit(rustc, expected_commit=str(toolchain["rustcCommitHash"]))
    _verify_rust_runtime(toolchain)
    windows_tools = _pinned_windows_build_tools(toolchain)
    profile = _windows_known_folder("5e6c858f-0e22-4760-9afe-ea3317b67173")
    rust_root = profile / ".rustup" / "toolchains" / str(toolchain["channel"])
    tool_locks = LockedToolchainTrees(
        [
            rust_root / "bin",
            rust_root / "lib",
            git.parents[2],
            windows_tools.msvc_root,
            windows_tools.sdk_bin.parent,
            windows_tools.sdk_include,
            windows_tools.sdk_lib,
        ]
    )
    build_root = staging / f".helper-build-{uuid.uuid4().hex[:16]}"
    build_root.mkdir(mode=0o700)
    build_lock = _OwnedDirectoryLock(build_root)
    build_root_identity = build_lock.identity
    binary_lock: LockedFile | None = None
    try:
        binary, returned_root, returned_identity, evidence = _build_release_helper_locked(
            build_root,
            build_root_identity,
            release_sha,
            toolchain,
            git,
            cargo,
            rustc,
            windows_tools,
        )
        if _git_sha_clean(git) != release_sha:
            raise ProofInputError("release checkout changed during helper build")
        binary_lock = LockedFile(binary, require_single_link=False)
        binary_hash, binary_size, _mode = _hash_stable_file(binary, require_single_link=False)
        _require_binary_git_marker(binary, release_sha, require_single_link=False)
        if _git_sha_clean(git) != release_sha:
            raise ProofInputError("release checkout changed before helper build acceptance")
        result = (
            binary,
            binary_lock,
            binary_hash,
            binary_size,
            returned_root,
            returned_identity,
            evidence,
        )
        binary_lock = None
        return result
    except Exception:
        if binary_lock is not None:
            binary_lock.close()
        raise
    finally:
        build_lock.close()
        tool_locks.close()


def _build_release_helper_locked(
    build_root: Path,
    build_root_identity: tuple[int, int],
    release_sha: str,
    toolchain: Mapping[str, Any],
    git: Path,
    cargo: Path,
    rustc: Path,
    windows_tools: WindowsBuildTools,
) -> tuple[Path, Path, tuple[int, int], dict[str, Any]]:
    build_temp = build_root / "temp"
    build_temp.mkdir(mode=0o700)
    project = _materialize_release_source(build_root, release_sha, git)
    source_cargo_home = _windows_known_folder("5e6c858f-0e22-4760-9afe-ea3317b67173") / ".cargo"
    cargo_config = _require_exact_cargo_configuration(
        cargo_home=source_cargo_home,
        release_sha=release_sha,
        git=git,
        expected_sha256=str(toolchain["cargoConfigSha256"]),
        project_root=project,
    )
    system32 = Path(_minimal_windows_build_environment()["SystemRoot"]) / "System32"
    build_path = os.pathsep.join(
        (
            os.fspath(windows_tools.msvc_bin),
            os.fspath(windows_tools.sdk_bin),
            os.fspath(cargo.parent),
            os.fspath(rustc.parent),
            os.fspath(git.parent),
            os.fspath(system32),
        )
    )
    source_date_epoch = _git_commit_timestamp(release_sha, git)
    reproducible_rustflags = "\x1f".join(
        (
            "-C",
            "target-feature=+crt-static",
            "-C",
            "link-arg=/Brepro",
            "--remap-path-prefix",
            f"{build_root}=C:/cortex-owner-proof-build",
        )
    )
    vendor_root = build_root / "vendor"
    vendor_environment = windows_tools.environment(_minimal_windows_build_environment())
    vendor_environment["CARGO_HOME"] = os.fspath(source_cargo_home)
    vendor_environment["PATH"] = build_path
    vendor_environment["RUSTC"] = os.fspath(rustc)
    vendor_environment["CARGO_NET_OFFLINE"] = "true"
    vendor_environment["TEMP"] = os.fspath(build_temp)
    vendor_environment["TMP"] = os.fspath(build_temp)
    _run_native_link_preflight(
        rustc,
        windows_tools.link,
        build_root,
        vendor_environment,
        lambda path, payload: _atomic_write(path, payload),
    )
    try:
        vendored = _run_contained(
            [
                os.fspath(cargo),
                "vendor",
                "--locked",
                "--offline",
                "--versioned-dirs",
                os.fspath(vendor_root),
                "--manifest-path",
                os.fspath(project / "Cargo.toml"),
            ],
            cwd=project,
            env=vendor_environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=1800,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ProofInputError("locked offline dependency vendoring could not complete") from error
    if vendored.returncode != 0:
        raise ProofInputError("locked offline dependency vendoring failed")
    isolated_home = build_root / "cargo-home"
    isolated_home.mkdir(mode=0o700)
    target_root = build_root / "target"
    environment = windows_tools.environment(_minimal_windows_build_environment())
    environment["CARGO_HOME"] = os.fspath(isolated_home)
    environment["CARGO_TARGET_DIR"] = os.fspath(target_root)
    environment["CARGO_INCREMENTAL"] = "0"
    environment["CARGO_NET_OFFLINE"] = "true"
    environment["CARGO_ENCODED_RUSTFLAGS"] = reproducible_rustflags
    environment["PATH"] = build_path
    environment["RUSTC"] = os.fspath(rustc)
    environment["SOURCE_DATE_EPOCH"] = source_date_epoch
    environment["ZERO_AR_DATE"] = "1"
    environment["TEMP"] = os.fspath(build_temp)
    environment["TMP"] = os.fspath(build_temp)
    try:
        completed = _run_contained(
            [
                os.fspath(cargo),
                "build",
                "--locked",
                "--offline",
                "--config",
                'source.crates-io.replace-with="vendored-sources"',
                "--config",
                f"source.vendored-sources.directory={json.dumps(vendor_root.as_posix())}",
                "--release",
                "--bin",
                "owner_proof_db",
                "--manifest-path",
                os.fspath(project / "Cargo.toml"),
            ],
            cwd=project,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=3600,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ProofInputError("isolated release helper build could not complete") from error
    if completed.returncode != 0:
        raise ProofInputError("isolated locked offline release helper build failed")
    if _git_sha_clean(git, repository=project.parents[1]) != release_sha:
        raise ProofInputError("detached release source changed during helper build")
    _verify_rust_runtime(toolchain)
    _pinned_git_tool(toolchain)
    _pinned_windows_build_tools(toolchain)
    binary = target_root / "release" / ("owner_proof_db.exe" if os.name == "nt" else "owner_proof_db")
    binary = _assert_safe_existing_file(
        binary,
        role="isolated release migration helper",
        reject_protected=False,
        reject_snapshot=True,
        require_single_link=False,
    )
    _require_binary_git_marker(binary, release_sha, require_single_link=False)
    return (
        binary,
        build_root,
        build_root_identity,
        {
            "mode": "clean-isolated-cargo-locked-offline",
            "releaseGitSha": release_sha,
            "sourceTreeSha": _git_tree_for_commit(release_sha, git),
            "cargoLocked": True,
            "cargoOffline": True,
            "isolatedTarget": True,
            "reproducibleBuildProtocol": REPRODUCIBLE_BUILD_PROTOCOL,
            "sourceDateEpoch": source_date_epoch,
            **_helper_toolchain_evidence(toolchain),
            "cargoConfigSha256": _hash_stable_file(cargo_config)[0],
        },
    )


def _file_specs(contract: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    return {str(item["role"]): dict(item) for item in contract["files"]}


def _bundle_path(root: Path, relative: str) -> Path:
    parsed = PurePosixPath(_relative_path(relative, context="bundle path"))
    result = root.joinpath(*parsed.parts)
    if not _is_within(result, root):
        raise ProofInputError("bundle path escaped its root")
    return result


def _compare_helper_inspection(
    helper_result: Mapping[str, Any],
    python_inspection: Mapping[str, Any],
    *,
    expected_hash: str,
    expected_schema: int,
    expected_schema_fingerprint: str,
    expected_segments: int,
    expected_distinct_paths: int,
    campaign: str,
) -> dict[str, Any]:
    if helper_result.get("operation") != "inspect" or helper_result.get("databaseSha256") != expected_hash:
        raise ProofInputError("helper inspected a database outside the exact hash authority")
    inspection = helper_result.get("inspection")
    if not isinstance(inspection, dict) or inspection != python_inspection:
        raise ProofInputError("Rust and Python database inspections disagree")
    _validate_inspection(
        inspection,
        expected_schema=expected_schema,
        expected_schema_fingerprint=expected_schema_fingerprint,
        expected_segments=expected_segments,
        expected_distinct_paths=expected_distinct_paths,
        campaign=campaign,
    )
    return dict(inspection)


def _require_helper_schema_contract(helper: Helper, *, schema: int, fingerprint: str) -> None:
    result = helper.schema_contract(expected_schema=schema)
    if (
        result.get("operation") != "schema-contract"
        or result.get("schemaVersion") != schema
        or result.get("schemaFingerprintSha256") != fingerprint
    ):
        raise ProofInputError("migration helper schema contract differs from the immutable release authority")


def _delete_owned_tree_windows(root: Path, expected_root_identity: tuple[int, int]) -> None:
    _platform_delete_owned_tree_windows(root, expected_root_identity)


def _remove_owned_staging(
    path: Path,
    parent: Path,
    prefix: str,
    expected_identity: tuple[int, int],
) -> None:
    _platform_remove_owned_staging(
        path,
        parent,
        prefix,
        expected_identity,
        windows_delete=_delete_owned_tree_windows,
    )


def prepare_bundle(
    *,
    contract_path: Path,
    sources: SourcePaths,
    output_root: Path,
    helper_factory: HelperFactory = _default_helper_factory,
    git_sha: str | None = None,
    expected_contract_sha256: str = RELEASE_CONTRACT_SHA256,
) -> dict[str, Any]:
    release_contract = expected_contract_sha256 == RELEASE_CONTRACT_SHA256
    if release_contract and (
        git_sha is not None
        or sources.migration_helper is not None
        or helper_factory is not _default_helper_factory
        or os.name != "nt"
    ):
        raise ProofInputError(
            "release proof preparation requires Windows, the clean checkout identity, the real helper, and an isolated build"
        )
    contract = load_contract(contract_path, expected_sha256=expected_contract_sha256)
    release_git = _pinned_git_tool(contract["helperToolchain"]) if release_contract else None
    release_sha = _git_sha_clean(release_git) if git_sha is None else git_sha
    if FULL_GIT_SHA.fullmatch(release_sha) is None:
        raise ProofInputError("release Git SHA must be 40 lowercase hexadecimal characters")
    output = _assert_safe_output_root(output_root, allow_existing=True)
    source_map = sources.by_role()
    specs = _file_specs(contract)
    validated_sources: dict[str, Path] = {}
    for role, source in source_map.items():
        safe = _assert_safe_existing_file(
            source,
            role=role,
            reject_protected=True,
            reject_snapshot=role in DATABASE_ROLES,
        )
        if safe.name != specs[role]["sourceBasename"]:
            raise ProofInputError(f"{role} has the wrong source filename")
        if _is_within(safe, output):
            raise ProofInputError("proof sources cannot live inside the output bundle")
        validated_sources[role] = safe
    helper_code_source = _assert_safe_existing_file(
        APP_ROOT / "src-tauri" / "src" / "bin" / "owner_proof_db.rs",
        role="migration helper source",
        reject_protected=False,
        reject_snapshot=True,
    )
    helper_source_sha256, _helper_source_size, _helper_source_mode = _hash_stable_file(helper_code_source)
    helper_repo_path = helper_code_source.relative_to(REPO_ROOT).as_posix()
    if helper_repo_path != HELPER_REPO_PATH:
        raise ProofInputError("migration helper source is outside its release path")
    if release_contract and helper_source_sha256 != _git_blob_sha256(
        release_sha,
        helper_repo_path,
        release_git,
    ):
        raise ProofInputError("migration helper source is not the exact release commit blob")
    for role in DATABASE_ROLES:
        _reject_sqlite_sidecars(validated_sources[role])

    prepare_mutex: NamedMutex | None = None
    parent_lock = _OwnedDirectoryLock(output.parent)
    normalized_output = _normalized_path(output)
    container_staging = output.parent / _deterministic_staging_name(STAGING_PREFIX, normalized_output)
    staging = container_staging / BUNDLE_DIR
    staging_identity: tuple[int, int] | None = None
    container_lock: _OwnedDirectoryLock | None = None
    staging_lock: _OwnedDirectoryLock | None = None
    content_locks: list[_OwnedDirectoryLock] = []
    helper_binary_lock: LockedFile | None = None
    try:
        prepare_mutex = NamedMutex("CortexOwnerProofPrepare", _normalized_path(output))
    except ProofInputError:
        parent_lock.close()
        raise
    try:
        if os.path.lexists(output):
            if os.path.lexists(container_staging):
                raise ProofInputError("published output and deterministic staging both exist")
            existing = validate_bundle(
                output / BUNDLE_DIR,
                helper_factory=helper_factory,
                expected_contract_sha256=expected_contract_sha256,
            )
            if existing.get("releaseGitSha") != release_sha:
                raise ProofInputError("preexisting proof bundle belongs to another release")
            return existing
        _recover_staging_transaction(
            container_staging,
            parent=output.parent,
            prefix=STAGING_PREFIX,
            kind="prepare",
            normalized_final_path=normalized_output,
            release_git_sha=release_sha,
            run_token=None,
            remove_unsealed=_remove_owned_staging,
        )
        container_staging.mkdir(mode=0o700)
        container_lock = _OwnedDirectoryLock(container_staging, publish=True)
        staging_identity = container_lock.identity
        transaction_owner = _begin_transaction(
            container_lock,
            kind="prepare",
            normalized_final_path=normalized_output,
            release_git_sha=release_sha,
            run_token=None,
            flush_directory=_fsync_directory,
        )
        staging.mkdir(mode=0o700)
        staging_lock = _OwnedDirectoryLock(staging, publish=True)
        verify_root = container_staging / VERIFY_ROOT_DIR
        verify_root.mkdir(mode=0o700)
        content_locks.append(_OwnedDirectoryLock(verify_root, publish=True))
        for relative in ("media", "audiobook", "db-authorities", "db-derived", "tools", ATTEMPTS_DIR):
            directory = staging / relative
            directory.mkdir(mode=0o700)
            content_locks.append(_OwnedDirectoryLock(directory, publish=True))
        helper_build_root: Path | None = None
        helper_build_root_identity: tuple[int, int] | None = None
        helper_built_hash: str | None = None
        helper_built_size: int | None = None
        if release_contract:
            (
                helper_binary_source,
                helper_binary_lock,
                helper_built_hash,
                helper_built_size,
                helper_build_root,
                helper_build_root_identity,
                helper_build,
            ) = _build_release_helper(staging, release_sha, contract["helperToolchain"], release_git)
        else:
            if sources.migration_helper is None:
                raise ProofInputError("synthetic proof tests require an explicit fake helper")
            helper_binary_source = _assert_safe_existing_file(
                sources.migration_helper,
                role="synthetic migration helper",
                reject_protected=True,
                reject_snapshot=True,
            )
            if helper_binary_source.name.casefold() != "owner_proof_db.exe":
                raise ProofInputError("migration helper must be named owner_proof_db.exe")
            _require_binary_git_marker(helper_binary_source, release_sha)
            helper_build = {
                "mode": "synthetic-test-override",
                "releaseGitSha": release_sha,
                "sourceTreeSha": release_sha,
                "cargoLocked": False,
                "cargoOffline": False,
                "isolatedTarget": False,
                "reproducibleBuildProtocol": "synthetic-test-override",
                "sourceDateEpoch": "0",
                **_helper_toolchain_evidence(contract["helperToolchain"]),
            }
        copied_files: list[dict[str, Any]] = []
        source_preservation: list[dict[str, Any]] = []
        for role in SOURCE_ROLES:
            spec = specs[role]
            destination = _bundle_path(staging, spec["relativePath"])
            copied = _copy_exact(
                validated_sources[role],
                destination,
                expected_sha256=spec["sha256"],
            )
            if "sizeBytes" in spec and copied["sizeBytes"] != spec["sizeBytes"]:
                raise ProofInputError(f"{role} size does not match the exact authority")
            _make_readonly(destination)
            copied_files.append(
                {
                    "role": role,
                    "relativePath": spec["relativePath"],
                    "sha256": copied["sha256"],
                    "sizeBytes": copied["sizeBytes"],
                    "readOnlyHashBound": True,
                }
            )
            source_preservation.append(
                {
                    "role": role,
                    "declaredSha256": spec["sha256"],
                    "copiedSha256": copied["sha256"],
                    "verifiedStableBeforeAndAfter": True,
                }
            )

        contract_bytes = canonical_json_bytes(contract)
        contract_destination = _bundle_path(staging, CONTRACT_BUNDLE_PATH)
        _atomic_write(contract_destination, contract_bytes, readonly=True)
        contract_hash = hashlib.sha256(contract_bytes).hexdigest()
        contract_size = len(contract_bytes)
        copied_files.append(
            {
                "role": "proof-input-contract",
                "relativePath": CONTRACT_BUNDLE_PATH,
                "sha256": contract_hash,
                "sizeBytes": contract_size,
                "readOnlyHashBound": True,
            }
        )

        helper_destination = _bundle_path(staging, HELPER_BUNDLE_PATH)
        helper_copy = _copy_exact(
            helper_binary_source,
            helper_destination,
            expected_sha256=helper_built_hash,
            source_require_single_link=not release_contract,
        )
        if helper_built_size is not None and helper_copy["sizeBytes"] != helper_built_size:
            raise ProofInputError("copied helper size differs from the retained build authority")
        if helper_binary_lock is not None:
            helper_binary_lock.verify()
            helper_binary_lock.close()
            helper_binary_lock = None
        _require_binary_git_marker(helper_destination, release_sha)
        if helper_build_root is not None:
            if helper_build_root_identity is None:
                raise ProofInputError("isolated helper build root lacks captured ownership identity")
            _remove_owned_staging(
                helper_build_root,
                staging,
                ".helper-build-",
                helper_build_root_identity,
            )
            if helper_build_root.exists():
                raise ProofInputError("isolated helper build target could not be removed safely")
        _make_readonly(helper_destination)
        copied_files.append(
            {
                "role": "database-migration-helper",
                "relativePath": HELPER_BUNDLE_PATH,
                "sha256": helper_copy["sha256"],
                "sizeBytes": helper_copy["sizeBytes"],
                "readOnlyHashBound": True,
            }
        )
        helper_code_destination = _bundle_path(staging, HELPER_SOURCE_BUNDLE_PATH)
        helper_code_copy = _copy_exact(
            helper_code_source,
            helper_code_destination,
            expected_sha256=helper_source_sha256,
        )
        _make_readonly(helper_code_destination)
        copied_files.append(
            {
                "role": "database-migration-helper-source",
                "relativePath": HELPER_SOURCE_BUNDLE_PATH,
                "sha256": helper_code_copy["sha256"],
                "sizeBytes": helper_code_copy["sizeBytes"],
                "readOnlyHashBound": True,
            }
        )
        helper = helper_factory(
            helper_destination,
            helper_copy["sha256"],
            release_sha,
            helper_code_copy["sha256"],
        )
        if (
            helper.git_sha != release_sha
            or helper.helper_sha256 != helper_copy["sha256"]
            or helper.helper_source_sha256 != helper_code_copy["sha256"]
        ):
            raise ProofInputError("migration helper factory did not preserve release identity")

        scale_contract = contract["databaseContracts"]["scale"]
        campaign_contract = contract["databaseContracts"]["campaignExact"]
        _require_helper_schema_contract(
            helper,
            schema=scale_contract["sourceSchemaVersion"],
            fingerprint=scale_contract["sourceSchemaFingerprintSha256"],
        )
        _require_helper_schema_contract(
            helper,
            schema=scale_contract["targetSchemaVersion"],
            fingerprint=scale_contract["targetSchemaFingerprintSha256"],
        )
        _require_helper_schema_contract(
            helper,
            schema=campaign_contract["schemaVersion"],
            fingerprint=campaign_contract["schemaFingerprintSha256"],
        )
        scale_authority = _bundle_path(staging, specs["scale-database-authority"]["relativePath"])
        campaign_authority = _bundle_path(staging, specs["campaign-database-authority"]["relativePath"])
        scale_python = inspect_sqlite_readonly(scale_authority)
        scale_helper = helper.inspect(
            scale_authority,
            expected_schema=scale_contract["sourceSchemaVersion"],
            campaign="absent",
        )
        scale_inspection = _compare_helper_inspection(
            scale_helper,
            scale_python,
            expected_hash=specs["scale-database-authority"]["sha256"],
            expected_schema=scale_contract["sourceSchemaVersion"],
            expected_schema_fingerprint=scale_contract["sourceSchemaFingerprintSha256"],
            expected_segments=scale_contract["segmentCount"],
            expected_distinct_paths=scale_contract["distinctAudioPathCount"],
            campaign="absent",
        )
        campaign_python = inspect_sqlite_readonly(campaign_authority)
        campaign_helper = helper.inspect(
            campaign_authority,
            expected_schema=campaign_contract["schemaVersion"],
            campaign="required",
        )
        campaign_inspection = _compare_helper_inspection(
            campaign_helper,
            campaign_python,
            expected_hash=specs["campaign-database-authority"]["sha256"],
            expected_schema=campaign_contract["schemaVersion"],
            expected_schema_fingerprint=campaign_contract["schemaFingerprintSha256"],
            expected_segments=campaign_contract["segmentCount"],
            expected_distinct_paths=campaign_contract["distinctAudioPathCount"],
            campaign="required",
        )

        derived_relative = scale_contract["derivedRelativePath"]
        derived_final = _bundle_path(staging, derived_relative)
        derived_work = derived_final.with_name(f"{derived_final.stem}.work.db")
        if not derived_work.name.endswith(".work.db"):
            raise ProofInputError("derived migration workspace suffix is invalid")
        derived_work.parent.mkdir(parents=True, exist_ok=True)
        if derived_work.exists():
            raise ProofInputError("derived migration workspace already exists")
        migration = helper.migrate(
            scale_authority,
            derived_work,
            staging_root=staging,
            source_sha256=specs["scale-database-authority"]["sha256"],
            expected_source_schema=scale_contract["sourceSchemaVersion"],
            expected_target_schema=scale_contract["targetSchemaVersion"],
        )
        if (
            migration.get("operation") != "migrate"
            or migration.get("sourceSha256") != specs["scale-database-authority"]["sha256"]
            or migration.get("appliedMigrations")
            != list(range(scale_contract["sourceSchemaVersion"] + 1, scale_contract["targetSchemaVersion"] + 1))
        ):
            raise ProofInputError("migration helper did not prove the exact schema transition")
        _reject_sqlite_sidecars(derived_work)
        if derived_final.exists():
            raise ProofInputError("derived baseline destination already exists")
        _publish_file_without_overwrite(derived_work, derived_final)
        derived_hash, derived_size, _mode = _hash_stable_file(derived_final)
        if migration.get("resultSha256") != derived_hash:
            raise ProofInputError("migration result hash disagrees with the published derivative")
        derived_python = inspect_sqlite_readonly(derived_final)
        after = migration.get("after")
        if not isinstance(after, dict) or after != derived_python:
            raise ProofInputError("Rust and Python disagree on the migrated scale derivative")
        _validate_inspection(
            after,
            expected_schema=scale_contract["targetSchemaVersion"],
            expected_schema_fingerprint=scale_contract["targetSchemaFingerprintSha256"],
            expected_segments=scale_contract["segmentCount"],
            expected_distinct_paths=scale_contract["distinctAudioPathCount"],
            campaign="absent",
        )
        _make_readonly(derived_final)
        copied_files.append(
            {
                "role": "scale-database-derived-current",
                "relativePath": derived_relative,
                "sha256": derived_hash,
                "sizeBytes": derived_size,
                "readOnlyHashBound": True,
            }
        )

        for role, source in validated_sources.items():
            observed, _size, _mode = _hash_stable_file(source)
            if observed != specs[role]["sha256"]:
                raise ProofInputError("a source authority changed before publication")
        for role in DATABASE_ROLES:
            _reject_sqlite_sidecars(validated_sources[role])

        manifest = {
            "schema": 1,
            "bundleId": contract["bundleId"],
            "releaseGitSha": release_sha,
            "contractSha256": contract_hash,
            "helperSha256": helper_copy["sha256"],
            "helperSourceSha256": helper_code_copy["sha256"],
            "helperBuild": helper_build,
            "files": sorted(copied_files, key=lambda item: (item["relativePath"], item["role"])),
            "sourcePreservation": sorted(source_preservation, key=lambda item: item["role"]),
            "databases": {
                "scaleAuthority": scale_inspection,
                "scaleDerived": {
                    **derived_python,
                    "authoritySha256": specs["scale-database-authority"]["sha256"],
                    "appliedMigrations": migration["appliedMigrations"],
                },
                "campaignExactAuthority": campaign_inspection,
            },
            "safety": {
                "sourcePathsPersisted": False,
                "liveAppDataAccepted": False,
                "snapshotAcceptedAsWritableAttempt": False,
                "campaignPolicyDeletedOrRewritten": False,
                "attemptDeletionPolicy": "manual-only-never-deleted-by-this-tool",
            },
        }
        _atomic_write(staging / MANIFEST_NAME, canonical_json_bytes(manifest), readonly=True)
        _validate_bundle_locked(
            staging,
            helper_factory=helper_factory,
            allow_staging=True,
            expected_contract_sha256=expected_contract_sha256,
        )
        _publish_sealed_directory(
            container_lock,
            [staging_lock, *content_locks],
            [staging / MANIFEST_NAME, *(_bundle_path(staging, item["relativePath"]) for item in copied_files)],
            output,
            _fsync_directory,
            seal_root_deletion=True,
            child_permissions={
                BUNDLE_DIR: 0x2 | 0x4 | 0x40,
                VERIFY_ROOT_DIR: 0x2 | 0x40,
                f"{BUNDLE_DIR}/{ATTEMPTS_DIR}": 0x2 | 0x40,
            },
            recovery_plan_callback=lambda entries: _record_seal_plan(
                container_lock,
                transaction_owner,
                entries,
                _fsync_directory,
            ),
            allowed_unsealed_recovery_paths=PUBLISHED_TRANSACTION_FILES,
        )
        _validate_published_transaction(
            output,
            kind="prepare",
            normalized_final_path=normalized_output,
            release_git_sha=release_sha,
            run_token=None,
        )
        container_lock.close()
        container_lock = None
        staging_lock = None
        content_locks.clear()
        return manifest
    except Exception:
        if helper_binary_lock is not None:
            helper_binary_lock.close()
            helper_binary_lock = None
        for locked in reversed(content_locks):
            locked.close()
        content_locks.clear()
        if staging_lock is not None:
            staging_lock.close()
            staging_lock = None
        if container_lock is not None:
            container_lock.close()
            container_lock = None
        if staging_identity is not None:
            _recover_staging_transaction(
                container_staging,
                parent=output.parent,
                prefix=STAGING_PREFIX,
                kind="prepare",
                normalized_final_path=normalized_output,
                release_git_sha=release_sha,
                run_token=None,
                remove_unsealed=_remove_owned_staging,
            )
        raise
    finally:
        if helper_binary_lock is not None:
            helper_binary_lock.close()
        for locked in reversed(content_locks):
            locked.close()
        if staging_lock is not None:
            staging_lock.close()
        if container_lock is not None:
            container_lock.close()
        if prepare_mutex is not None:
            prepare_mutex.close()
        parent_lock.close()


def _validation_api() -> Any:
    """Return this live facade so test monkeypatches remain runtime authorities."""
    return sys.modules[__name__]


def _manifest_files(manifest: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    return _owner_proof_validation.manifest_files(_validation_api(), manifest)


def _assert_manifest_has_no_private_paths(value: Any) -> None:
    _owner_proof_validation.assert_manifest_has_no_private_paths(_validation_api(), value)


def _assert_bundle_inventory(root: Path, expected_files: set[str]) -> None:
    _owner_proof_validation.assert_bundle_inventory(_validation_api(), root, expected_files)


def _verify_release_helper_rebuild(
    bundle: Path,
    contract: Mapping[str, Any],
    manifest: Mapping[str, Any],
    helper_entry: Mapping[str, Any],
    release_git: Path,
) -> None:
    _owner_proof_validation.verify_release_helper_rebuild(
        _validation_api(),
        bundle,
        contract,
        manifest,
        helper_entry,
        release_git,
    )


def _require_directory_denials(path: Path, rights: tuple[int, ...], *, context: str) -> None:
    _owner_proof_validation.require_directory_denials(
        _validation_api(),
        path,
        rights,
        context=context,
    )


def _proof_container(bundle: Path) -> tuple[Path, Path]:
    return _owner_proof_validation.proof_container(_validation_api(), bundle)


def _require_container_namespace_seals(container: Path, bundle: Path, verify_root: Path) -> None:
    _owner_proof_validation.require_container_namespace_seals(
        _validation_api(),
        container,
        bundle,
        verify_root,
    )


def _require_bundle_namespace_seals(bundle: Path) -> None:
    _owner_proof_validation.require_bundle_namespace_seals(_validation_api(), bundle)


def validate_bundle(
    root: Path,
    *,
    helper_factory: HelperFactory = _default_helper_factory,
    allow_staging: bool = False,
    expected_contract_sha256: str = RELEASE_CONTRACT_SHA256,
) -> dict[str, Any]:
    """Validate one stable bundle namespace while all mutable child directory names are locked."""
    return _owner_proof_validation.validate_bundle(
        _validation_api(),
        root,
        helper_factory=helper_factory,
        allow_staging=allow_staging,
        expected_contract_sha256=expected_contract_sha256,
    )


def _validate_bundle_locked(
    root: Path,
    *,
    helper_factory: HelperFactory = _default_helper_factory,
    allow_staging: bool = False,
    expected_contract_sha256: str = RELEASE_CONTRACT_SHA256,
    require_namespace_seals: bool = False,
) -> dict[str, Any]:
    return _owner_proof_validation.validate_bundle_locked(
        _validation_api(),
        root,
        helper_factory=helper_factory,
        allow_staging=allow_staging,
        expected_contract_sha256=expected_contract_sha256,
        require_namespace_seals=require_namespace_seals,
    )


def _canonical_run_token(value: str) -> str:
    return _owner_proof_validation.canonical_run_token(_validation_api(), value)


def _attempt_result(
    bundle: Path,
    final: Path,
    token: str,
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    return _owner_proof_validation.attempt_result(
        _validation_api(),
        bundle,
        final,
        token,
        files,
    )


def _build_attempt_manifest(
    token: str,
    manifest: Mapping[str, Any],
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    return _owner_proof_validation.build_attempt_manifest(
        _validation_api(),
        token,
        manifest,
        files,
    )


def _recover_attempt(
    bundle: Path,
    final: Path,
    token: str,
    manifest: Mapping[str, Any],
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    return _owner_proof_validation.recover_attempt(
        _validation_api(),
        bundle,
        final,
        token,
        manifest,
        files,
    )


def create_attempt(
    *,
    bundle_root: Path,
    run_token: str,
    helper_factory: HelperFactory = _default_helper_factory,
    expected_contract_sha256: str = RELEASE_CONTRACT_SHA256,
) -> dict[str, Any]:
    token = _canonical_run_token(run_token)
    bundle = _assert_no_links(bundle_root)
    container, verify_root = _proof_container(bundle)
    container_lock = _OwnedDirectoryLock(container)
    try:
        bundle_lock = _OwnedDirectoryLock(bundle)
    except Exception:
        container_lock.close()
        raise
    try:
        verify_lock = _OwnedDirectoryLock(verify_root, pin_namespace=False)
    except Exception:
        bundle_lock.close()
        container_lock.close()
        raise
    attempts_lock: _OwnedDirectoryLock | None = None
    attempt_mutex: NamedMutex | None = None
    content_locks: list[_OwnedDirectoryLock] = []
    attempts = bundle / ATTEMPTS_DIR
    final = attempts / token
    normalized_final = _normalized_path(final)
    attempt_staging_prefix = f"{ATTEMPT_STAGING_PREFIX}{token}.staging"
    staging = attempts / attempt_staging_prefix
    try:
        _require_container_namespace_seals(container, bundle, verify_root)
        _require_bundle_namespace_seals(bundle)
        for name in ("media", "audiobook", "db-authorities", "db-derived", "tools"):
            child = bundle / name
            content_locks.append(_OwnedDirectoryLock(child, pin_namespace=False))
        attempts = _assert_no_links(attempts)
        attempts_lock = _OwnedDirectoryLock(attempts, pin_namespace=False)
        container_transaction = _validate_published_transaction(
            container,
            kind="prepare",
            normalized_final_path=_normalized_path(container),
            release_git_sha=None,
            run_token=None,
            mutable_descendant_roots=(VERIFY_ROOT_DIR, f"{BUNDLE_DIR}/{ATTEMPTS_DIR}"),
        )
        manifest = _validate_bundle_locked(
            bundle,
            helper_factory=helper_factory,
            expected_contract_sha256=expected_contract_sha256,
            require_namespace_seals=True,
        )
        if container_transaction["releaseGitSha"] != manifest["releaseGitSha"]:
            raise ProofInputError("publication transaction and bundle manifest release identities differ")
        _fsync_directory(container.parent)
        if not attempts.is_dir() or _is_snapshot_path(attempts):
            raise ProofInputError("attempt root is invalid or belongs to snapshot authority")
        attempt_mutex = NamedMutex(
            "CortexOwnerProofAttempt",
            f"{_normalized_path(bundle)}\0{token}",
        )
        files = _manifest_files(manifest)
        if os.path.lexists(final):
            if os.path.lexists(staging):
                raise ProofInputError("published attempt and deterministic staging both exist")
            final_lock = _OwnedDirectoryLock(final, pin_namespace=False)
            try:
                _validate_published_transaction(
                    final,
                    kind="attempt",
                    normalized_final_path=normalized_final,
                    release_git_sha=manifest["releaseGitSha"],
                    run_token=token,
                )
                result = _recover_attempt(bundle, final, token, manifest, files)
                # Reconcile a prior post-rename durability-unknown outcome only
                # after the published attempt has passed every identity/content
                # check, while its containing namespace remains locked.
                _fsync_attempts_directory(attempts)
            finally:
                final_lock.close()
            attempts_lock.close()
            for locked in reversed(content_locks):
                locked.close()
            verify_lock.close()
            bundle_lock.close()
            container_lock.close()
            attempt_mutex.close()
            attempt_mutex = None
            return result
        _recover_staging_transaction(
            staging,
            parent=attempts,
            prefix=attempt_staging_prefix,
            kind="attempt",
            normalized_final_path=normalized_final,
            release_git_sha=manifest["releaseGitSha"],
            run_token=token,
            remove_unsealed=_remove_owned_staging,
        )
        contract = validate_contract(
            _load_json(_bundle_path(bundle, files["proof-input-contract"]["relativePath"]), canonical=True)
        )
        scale_contract = contract["databaseContracts"]["scale"]
        campaign_contract = contract["databaseContracts"]["campaignExact"]
        helper_path = _bundle_path(bundle, files["database-migration-helper"]["relativePath"])
        helper = helper_factory(
            helper_path,
            manifest["helperSha256"],
            manifest["releaseGitSha"],
            manifest["helperSourceSha256"],
        )
    except Exception:
        if attempts_lock is not None:
            attempts_lock.close()
        if attempt_mutex is not None:
            attempt_mutex.close()
            attempt_mutex = None
        for locked in reversed(content_locks):
            locked.close()
        verify_lock.close()
        bundle_lock.close()
        container_lock.close()
        raise
    assert attempts_lock is not None
    staging_identity: tuple[int, int] | None = None
    staging_lock: _OwnedDirectoryLock | None = None
    try:
        staging.mkdir(mode=0o700)
        staging_lock = _OwnedDirectoryLock(staging, publish=True)
        staging_identity = staging_lock.identity
        transaction_owner = _begin_transaction(
            staging_lock,
            kind="attempt",
            normalized_final_path=normalized_final,
            release_git_sha=manifest["releaseGitSha"],
            run_token=token,
            flush_directory=_fsync_directory,
        )
        scale_target = staging / "scale-work.db"
        campaign_target = staging / "campaign-observation.db"
        scale_source = _bundle_path(bundle, files["scale-database-derived-current"]["relativePath"])
        campaign_source = _bundle_path(bundle, files["campaign-database-authority"]["relativePath"])
        scale_copy = _copy_exact(scale_source, scale_target, expected_sha256=files["scale-database-derived-current"]["sha256"])
        campaign_copy = _copy_exact(
            campaign_source,
            campaign_target,
            expected_sha256=files["campaign-database-authority"]["sha256"],
        )
        _make_writable(scale_target)
        _make_writable(campaign_target)
        scale_python = inspect_sqlite_readonly(scale_target)
        campaign_python = inspect_sqlite_readonly(campaign_target)
        _compare_helper_inspection(
            helper.inspect(
                scale_target,
                expected_schema=scale_contract["targetSchemaVersion"],
                campaign="absent",
            ),
            scale_python,
            expected_hash=scale_copy["sha256"],
            expected_schema=scale_contract["targetSchemaVersion"],
            expected_schema_fingerprint=scale_contract["targetSchemaFingerprintSha256"],
            expected_segments=scale_contract["segmentCount"],
            expected_distinct_paths=scale_contract["distinctAudioPathCount"],
            campaign="absent",
        )
        _compare_helper_inspection(
            helper.inspect(
                campaign_target,
                expected_schema=campaign_contract["schemaVersion"],
                campaign="required",
            ),
            campaign_python,
            expected_hash=campaign_copy["sha256"],
            expected_schema=campaign_contract["schemaVersion"],
            expected_schema_fingerprint=campaign_contract["schemaFingerprintSha256"],
            expected_segments=campaign_contract["segmentCount"],
            expected_distinct_paths=campaign_contract["distinctAudioPathCount"],
            campaign="required",
        )
        attempt_manifest = _build_attempt_manifest(token, manifest, files)
        attempt_manifest_path = staging / "attempt-manifest.v1.json"
        _atomic_write(attempt_manifest_path, canonical_json_bytes(attempt_manifest), readonly=True)
        result = _attempt_result(bundle, final, token, files)
        _publish_sealed_directory(
            staging_lock,
            [],
            [scale_target, campaign_target, attempt_manifest_path],
            final,
            _fsync_attempts_directory,
            seal_root_deletion=True,
            root_child_permissions=0x4 | 0x40,
            recovery_plan_callback=lambda entries: _record_seal_plan(
                staging_lock,
                transaction_owner,
                entries,
                _fsync_directory,
            ),
            allowed_unsealed_recovery_paths=PUBLISHED_TRANSACTION_FILES,
        )
        _validate_published_transaction(
            final,
            kind="attempt",
            normalized_final_path=normalized_final,
            release_git_sha=manifest["releaseGitSha"],
            run_token=token,
        )
        return result
    except Exception:
        if staging_lock is not None:
            staging_lock.close()
            staging_lock = None
        if staging_identity is not None:
            _recover_staging_transaction(
                staging,
                parent=attempts,
                prefix=attempt_staging_prefix,
                kind="attempt",
                normalized_final_path=normalized_final,
                release_git_sha=manifest["releaseGitSha"],
                run_token=token,
                remove_unsealed=_remove_owned_staging,
            )
        raise
    finally:
        if staging_lock is not None:
            staging_lock.close()
        attempts_lock.close()
        if attempt_mutex is not None:
            attempt_mutex.close()
        for locked in reversed(content_locks):
            locked.close()
        verify_lock.close()
        bundle_lock.close()
        container_lock.close()


def main(argv: list[str] | None = None) -> int:
    from owner_proof_cli import run_cli

    return run_cli(__import__(__name__), argv)


if __name__ == "__main__":
    raise SystemExit(main())
