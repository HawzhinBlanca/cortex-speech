#!/usr/bin/env python3
"""Crash-recoverable transaction journals for owner-proof directory publication.

The journals contain no source paths.  They bind one deterministic staging namespace to its
intended final-name digest, release identity, root file identity, and the exact ACL recovery plan
captured before publication starts mutating DACLs.  They are recovery metadata, not proof-data
authority: corruption or an unexpected name always fails closed.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
from pathlib import Path
from typing import Any, Callable, Collection, Mapping, Sequence

from owner_proof_contract import FULL_GIT_SHA, canonical_json_bytes, expect_keys, parse_json_bytes
from owner_proof_platform import (
    FILE_ATTRIBUTE_REPARSE_POINT,
    LockedFile,
    OwnedDirectoryLock,
    ProofInputError,
    PublicationRecoveryEntry,
    recover_owned_publication_staging,
    validate_owned_publication_plan,
)


OWNER_JOURNAL_NAME = ".owner-proof-owner.v1.json"
OWNER_TEMP_NAME = f"{OWNER_JOURNAL_NAME}.tmp"
SEAL_PLAN_NAME = ".owner-proof-seal-plan.v1.json"
SEAL_PLAN_TEMP_NAME = f"{SEAL_PLAN_NAME}.tmp"
PUBLISHED_TRANSACTION_FILES = frozenset((OWNER_JOURNAL_NAME, SEAL_PLAN_NAME))
RECOVERY_METADATA_DELETE_ORDER = (SEAL_PLAN_NAME, OWNER_JOURNAL_NAME)
_RECOVERY_TRANSACTION_FILES = frozenset(
    (OWNER_JOURNAL_NAME, OWNER_TEMP_NAME, SEAL_PLAN_NAME, SEAL_PLAN_TEMP_NAME)
)
_LOWER_SHA256 = re.compile(r"[0-9a-f]{64}")
_MAX_JOURNAL_BYTES = 2 * 1024 * 1024


def final_path_sha256(normalized_final_path: str) -> str:
    if not isinstance(normalized_final_path, str) or not normalized_final_path:
        raise ProofInputError("transaction final path identity is empty")
    return hashlib.sha256(
        b"cortex-owner-proof-final-path-v1\x00" + normalized_final_path.encode("utf-8", errors="strict")
    ).hexdigest()


def deterministic_staging_name(prefix: str, normalized_final_path: str) -> str:
    if not prefix or "/" in prefix or "\\" in prefix or "\x00" in prefix:
        raise ProofInputError("transaction staging prefix is invalid")
    return f"{prefix}{final_path_sha256(normalized_final_path)[:32]}"


def _identity_json(identity: tuple[int, int]) -> list[int]:
    if (
        not isinstance(identity, tuple)
        or len(identity) != 2
        or any(isinstance(value, bool) or not isinstance(value, int) or value < 0 for value in identity)
    ):
        raise ProofInputError("transaction root identity is invalid")
    return [identity[0], identity[1]]


def _parse_identity(value: Any, *, context: str) -> tuple[int, int]:
    if (
        not isinstance(value, list)
        or len(value) != 2
        or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 for item in value)
    ):
        raise ProofInputError(f"{context} identity is invalid")
    return (value[0], value[1])


def _metadata_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & FILE_ATTRIBUTE_REPARSE_POINT)


def _canonical_relative(value: Any, *, context: str) -> str:
    if value == ".":
        return "."
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise ProofInputError(f"{context} path is not canonical")
    parts = value.split("/")
    if any(part in ("", ".", "..") for part in parts) or Path(value).is_absolute():
        raise ProofInputError(f"{context} path is not canonical")
    return value


def _journal_bytes(path: Path) -> bytes:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise ProofInputError("transaction journal metadata cannot be read") from error
    size = metadata.st_size
    if size <= 0 or size > _MAX_JOURNAL_BYTES:
        raise ProofInputError("transaction journal size is invalid")
    if os.name == "nt":
        readonly = bool(getattr(metadata, "st_file_attributes", 0) & 0x1)
    else:
        readonly = not bool(stat.S_IMODE(metadata.st_mode) & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH))
    if not readonly:
        raise ProofInputError("transaction journal must remain read-only")
    with LockedFile(path) as locked:
        if (metadata.st_dev, metadata.st_ino) != locked.identity or metadata.st_nlink != 1:
            raise ProofInputError("transaction journal changed between namespace check and identity lock")
        try:
            payload = path.read_bytes()
        except OSError as error:
            raise ProofInputError("transaction journal cannot be read") from error
        locked.verify()
    if len(payload) != size:
        raise ProofInputError("transaction journal changed while it was read")
    return payload


def _load_canonical_journal(path: Path) -> tuple[dict[str, Any], bytes]:
    payload = _journal_bytes(path)
    value = parse_json_bytes(payload, context="transaction journal")
    if not isinstance(value, dict) or payload != canonical_json_bytes(value):
        raise ProofInputError("transaction journal is not in canonical byte form")
    return value, payload


def _publish_journal_no_replace(
    root_lock: OwnedDirectoryLock,
    name: str,
    value: Mapping[str, Any],
    flush_directory: Callable[[Path], None],
) -> Path:
    if os.name != "nt":
        raise ProofInputError("owner-proof transaction publication requires Windows")
    root_lock.verify_path()
    path = root_lock.path / name
    temporary = root_lock.path / f"{name}.tmp"
    if os.path.lexists(path) or os.path.lexists(temporary):
        raise ProofInputError("transaction journal namespace is already occupied")
    payload = canonical_json_bytes(dict(value))
    descriptor = -1
    try:
        descriptor = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0),
            0o600,
        )
        with os.fdopen(descriptor, "wb", closefd=True) as target:
            descriptor = -1
            target.write(payload)
            target.flush()
            os.fsync(target.fileno())
        # Windows rename is atomic and refuses a pre-existing destination.  The root handle denies
        # ancestor replacement for the entire create/write/rename sequence.
        os.rename(temporary, path)
        os.chmod(path, stat.S_IREAD)
        flush_directory(root_lock.path)
        root_lock.verify_path()
        observed, observed_payload = _load_canonical_journal(path)
        if observed != dict(value) or observed_payload != payload:
            raise ProofInputError("transaction journal changed during publication")
        return path
    except Exception:
        if descriptor >= 0:
            os.close(descriptor)
        raise


def begin_transaction(
    root_lock: OwnedDirectoryLock,
    *,
    kind: str,
    normalized_final_path: str,
    release_git_sha: str,
    run_token: str | None,
    flush_directory: Callable[[Path], None],
) -> dict[str, Any]:
    if kind not in ("prepare", "attempt"):
        raise ProofInputError("transaction kind is invalid")
    if FULL_GIT_SHA.fullmatch(release_git_sha) is None:
        raise ProofInputError("transaction release Git SHA is invalid")
    if kind == "prepare" and run_token is not None:
        raise ProofInputError("prepare transaction cannot carry a run token")
    if kind == "attempt" and (not isinstance(run_token, str) or not run_token):
        raise ProofInputError("attempt transaction requires a run token")
    with os.scandir(root_lock.path) as scanned:
        if any(True for _entry in scanned):
            raise ProofInputError("new transaction root is not empty")
    state = {
        "schema": 1,
        "kind": kind,
        "finalPathSha256": final_path_sha256(normalized_final_path),
        "releaseGitSha": release_git_sha,
        "runToken": run_token,
        "rootIdentity": _identity_json(root_lock.identity),
    }
    _publish_journal_no_replace(root_lock, OWNER_JOURNAL_NAME, state, flush_directory)
    return state


def _entry_json(entry: PublicationRecoveryEntry) -> dict[str, Any]:
    relative = _canonical_relative(entry.relative_path, context="recovery entry")
    if entry.link_count != 1:
        raise ProofInputError("publication recovery entry is hardlinked")
    if _LOWER_SHA256.fullmatch(entry.protected_dacl_sha256) is None:
        raise ProofInputError("publication recovery DACL fingerprint is invalid")
    if (
        not isinstance(entry.deny_masks, tuple)
        or not entry.deny_masks
        or any(isinstance(mask, bool) or not isinstance(mask, int) or mask <= 0 or mask > 0xFFFFFFFF for mask in entry.deny_masks)
    ):
        raise ProofInputError("publication recovery cumulative deny states are invalid")
    return {
        "relativePath": relative,
        "identity": _identity_json(entry.identity),
        "directory": entry.is_directory,
        "linkCount": entry.link_count,
        "protectedDaclSha256": entry.protected_dacl_sha256,
        "cumulativeDenyMasks": list(entry.deny_masks),
    }


def record_seal_plan(
    root_lock: OwnedDirectoryLock,
    owner_state: Mapping[str, Any],
    entries: Sequence[PublicationRecoveryEntry],
    flush_directory: Callable[[Path], None],
) -> dict[str, Any]:
    root_lock.verify_path()
    owner, owner_payload = _load_canonical_journal(root_lock.path / OWNER_JOURNAL_NAME)
    if owner != dict(owner_state) or _parse_identity(owner.get("rootIdentity"), context="transaction owner") != root_lock.identity:
        raise ProofInputError("transaction owner journal changed before seal planning")
    serialized = [_entry_json(entry) for entry in entries]
    serialized.sort(key=lambda item: (item["relativePath"] != ".", item["relativePath"].casefold()))
    paths = [item["relativePath"] for item in serialized]
    if not paths or paths[0] != "." or len({path.casefold() for path in paths}) != len(paths):
        raise ProofInputError("transaction recovery plan inventory is invalid")
    if _parse_identity(serialized[0]["identity"], context="transaction plan root") != root_lock.identity:
        raise ProofInputError("transaction recovery plan root identity differs from its retained handle")
    plan = {
        "schema": 1,
        "ownerJournalSha256": hashlib.sha256(owner_payload).hexdigest(),
        "entries": serialized,
    }
    _publish_journal_no_replace(root_lock, SEAL_PLAN_NAME, plan, flush_directory)
    return plan


def _parse_owner(
    value: Mapping[str, Any],
    *,
    kind: str,
    normalized_final_path: str,
    release_git_sha: str | None,
    run_token: str | None,
    root_identity: tuple[int, int],
) -> dict[str, Any]:
    expect_keys(
        value,
        {"schema", "kind", "finalPathSha256", "releaseGitSha", "runToken", "rootIdentity"},
        context="transaction owner journal",
    )
    if (
        value["schema"] != 1
        or value["kind"] != kind
        or value["finalPathSha256"] != final_path_sha256(normalized_final_path)
        or FULL_GIT_SHA.fullmatch(str(value["releaseGitSha"])) is None
        or (release_git_sha is not None and value["releaseGitSha"] != release_git_sha)
        or value["runToken"] != run_token
        or _parse_identity(value["rootIdentity"], context="transaction owner") != root_identity
    ):
        raise ProofInputError("transaction owner journal does not match this exact operation")
    return dict(value)


def _parse_plan(value: Mapping[str, Any], owner_payload: bytes) -> list[PublicationRecoveryEntry]:
    expect_keys(value, {"schema", "ownerJournalSha256", "entries"}, context="transaction seal plan")
    if (
        value["schema"] != 1
        or value["ownerJournalSha256"] != hashlib.sha256(owner_payload).hexdigest()
        or not isinstance(value["entries"], list)
        or not value["entries"]
    ):
        raise ProofInputError("transaction seal plan does not match its owner journal")
    entries: list[PublicationRecoveryEntry] = []
    folded: set[str] = set()
    for raw in value["entries"]:
        if not isinstance(raw, dict):
            raise ProofInputError("transaction seal plan entry is not an object")
        expect_keys(
            raw,
            {
                "relativePath",
                "identity",
                "directory",
                "linkCount",
                "protectedDaclSha256",
                "cumulativeDenyMasks",
            },
            context="transaction seal plan entry",
        )
        relative = _canonical_relative(raw["relativePath"], context="transaction seal plan entry")
        if relative.casefold() in folded:
            raise ProofInputError("transaction seal plan contains a case-colliding path")
        folded.add(relative.casefold())
        masks = raw["cumulativeDenyMasks"]
        if (
            raw["linkCount"] != 1
            or not isinstance(raw["directory"], bool)
            or _LOWER_SHA256.fullmatch(str(raw["protectedDaclSha256"])) is None
            or not isinstance(masks, list)
            or not masks
            or any(type(mask) is not int or mask <= 0 or mask > 0xFFFFFFFF for mask in masks)
            or len(set(masks)) != len(masks)
            or any(current | previous != current for previous, current in zip(masks, masks[1:]))
        ):
            raise ProofInputError("transaction seal plan entry fields are invalid")
        entries.append(
            PublicationRecoveryEntry(
                relative_path=relative,
                identity=_parse_identity(raw["identity"], context="transaction seal plan entry"),
                is_directory=raw["directory"],
                link_count=raw["linkCount"],
                protected_dacl_sha256=raw["protectedDaclSha256"],
                deny_masks=tuple(masks),
            )
        )
    entries.sort(key=lambda item: (item.relative_path != ".", item.relative_path.casefold()))
    if entries[0].relative_path != ".":
        raise ProofInputError("transaction seal plan lacks its root entry")
    return entries


def _direct_tree(root: Path) -> dict[str, tuple[Path, bool, tuple[int, int], int]]:
    observed: dict[str, tuple[Path, bool, tuple[int, int], int]] = {}
    pending = [root]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as scanned:
            names = [entry.name for entry in scanned]
        if len({name.casefold() for name in names}) != len(names):
            raise ProofInputError("transaction tree contains a case-colliding name")
        for name in names:
            path = directory / name
            metadata = os.lstat(path)
            if stat.S_ISLNK(metadata.st_mode) or _metadata_reparse(metadata):
                raise ProofInputError("transaction tree contains a symlink or reparse point")
            is_directory = stat.S_ISDIR(metadata.st_mode)
            if not is_directory and not stat.S_ISREG(metadata.st_mode):
                raise ProofInputError("transaction tree contains a special entry")
            relative = path.relative_to(root).as_posix()
            observed[relative] = (
                path,
                is_directory,
                (metadata.st_dev, metadata.st_ino),
                getattr(metadata, "st_nlink", 1),
            )
            if is_directory:
                pending.append(path)
    return observed


def _allow_empty_or_owner_temp_reclaim(root: Path, tree: Mapping[str, tuple[Path, bool, tuple[int, int], int]]) -> bool:
    if not tree:
        return True
    if set(tree) != {OWNER_TEMP_NAME}:
        return False
    path, is_directory, _identity, links = tree[OWNER_TEMP_NAME]
    try:
        size = os.lstat(path).st_size
    except OSError:
        return False
    return not is_directory and links == 1 and 0 <= size <= _MAX_JOURNAL_BYTES


def recover_staging_transaction(
    root: Path,
    *,
    parent: Path,
    prefix: str,
    kind: str,
    normalized_final_path: str,
    release_git_sha: str,
    run_token: str | None,
    remove_unsealed: Callable[[Path, Path, str, tuple[int, int]], None],
) -> bool:
    """Recover exactly one deterministic staging root; never delete a published final name."""
    if not os.path.lexists(root):
        return False
    root_lock = OwnedDirectoryLock(root)
    identity = root_lock.identity
    try:
        tree = _direct_tree(root)
        if OWNER_JOURNAL_NAME not in tree:
            if not _allow_empty_or_owner_temp_reclaim(root, tree):
                raise ProofInputError("unowned deterministic staging namespace is occupied")
            mode = "unsealed"
            entries: list[PublicationRecoveryEntry] = []
        else:
            owner, owner_payload = _load_canonical_journal(root / OWNER_JOURNAL_NAME)
            _parse_owner(
                owner,
                kind=kind,
                normalized_final_path=normalized_final_path,
                release_git_sha=release_git_sha,
                run_token=run_token,
                root_identity=identity,
            )
            if OWNER_TEMP_NAME in tree:
                raise ProofInputError("transaction owner temporary file survived after owner publication")
            if SEAL_PLAN_TEMP_NAME in tree:
                # The seal callback cannot return while its deterministic temporary name exists, so
                # no ACL seal can have started in this state.
                mode = "unsealed"
                entries = []
            elif SEAL_PLAN_NAME not in tree:
                mode = "unsealed"
                entries = []
            else:
                plan, _plan_payload = _load_canonical_journal(root / SEAL_PLAN_NAME)
                entries = _parse_plan(plan, owner_payload)
                if entries[0].identity != identity:
                    raise ProofInputError("transaction seal plan root identity changed")
                mode = "planned"
    finally:
        root_lock.close()
    if mode == "planned":
        recover_owned_publication_staging(
            root,
            identity,
            entries,
            recovery_metadata_delete_order=RECOVERY_METADATA_DELETE_ORDER,
        )
    else:
        remove_unsealed(root, parent, prefix, identity)
    if os.path.lexists(root):
        raise ProofInputError("deterministic transaction staging root survived recovery")
    return True


def validate_published_transaction(
    root: Path,
    *,
    kind: str,
    normalized_final_path: str,
    release_git_sha: str | None,
    run_token: str | None,
    mutable_descendant_roots: Collection[str] = (),
) -> dict[str, Any]:
    """Validate retained recovery metadata and the exact identity inventory without mutation."""
    with OwnedDirectoryLock(root) as root_lock:
        tree = _direct_tree(root)
        if OWNER_TEMP_NAME in tree or SEAL_PLAN_TEMP_NAME in tree:
            raise ProofInputError("published transaction contains an unfinished journal temporary")
        if not PUBLISHED_TRANSACTION_FILES.issubset(tree):
            raise ProofInputError("published transaction recovery metadata is incomplete")
        owner, owner_payload = _load_canonical_journal(root / OWNER_JOURNAL_NAME)
        parsed_owner = _parse_owner(
            owner,
            kind=kind,
            normalized_final_path=normalized_final_path,
            release_git_sha=release_git_sha,
            run_token=run_token,
            root_identity=root_lock.identity,
        )
        plan, _plan_payload = _load_canonical_journal(root / SEAL_PLAN_NAME)
        entries = _parse_plan(plan, owner_payload)
        planned = {entry.relative_path: entry for entry in entries if entry.relative_path != "."}
        if entries[0].identity != root_lock.identity:
            raise ProofInputError("published transaction root differs from its seal plan")
        mutable_roots = {
            _canonical_relative(value, context="transaction mutable root") for value in mutable_descendant_roots
        }
        if not mutable_roots.issubset(planned) or any(not planned[value].is_directory for value in mutable_roots):
            raise ProofInputError("published transaction mutable root is not a planned directory")
        expected = set(planned) | set(PUBLISHED_TRANSACTION_FILES)
        if not expected.issubset(tree):
            raise ProofInputError("published transaction is missing a planned entry")
        unexpected = set(tree) - expected
        if any(not any(path.startswith(f"{root}/") for root in mutable_roots) for path in unexpected):
            raise ProofInputError("published transaction inventory differs from its exact seal plan")
        file_identities: set[tuple[int, int]] = set()
        for relative, entry in planned.items():
            _path, is_directory, identity, links = tree[relative]
            if is_directory != entry.is_directory or identity != entry.identity or links != entry.link_count:
                raise ProofInputError("published transaction entry differs from its exact seal plan")
            if not is_directory:
                if identity in file_identities:
                    raise ProofInputError("published transaction contains an aliased file identity")
                file_identities.add(identity)
        validate_owned_publication_plan(root, root_lock.identity, entries)
        return parsed_owner
