#!/usr/bin/env python3
"""Create and verify an atomic Cortex recovery snapshot without starting the app.

The desktop app normally creates SQLite online backups, but production policy changes must be
recoverable even while the shipped EXE is deliberately held offline.  This command mirrors the
app snapshot contract: SQLite's backup API captures the WAL-consistent database, queue/model state
travels beside it, a SHA-256 manifest is written before promotion, and an optional second-drive copy
is independently verified.

Run from the repository root::

    python cortex-speech-app/scripts/create_recovery_snapshot.py \
      --label pre_compensation_v1 \
      --expected-foreign-key-violations 4208

An expected non-zero FK count is permitted only for a preservation snapshot of an already-known
incident.  It is recorded in the manifest and must match exactly; a certified recovery snapshot
should use the default of zero.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import unicodedata
from pathlib import Path, PureWindowsPath

from activate_review_pilot import acquire_cortex_lock
from check_database_integrity import DEFAULT_MIGRATIONS, source_migrations
from pilot_focus_contract import verify_controlled_pilot_focus

DB_FILE = "cortex-speech.db"
# Each state has a mandatory representation: the real JSON file or its exact absence marker.
REQUIRED_STATE = (
    "settings.json",
    "champion.json",
    "reviewer_dialects.json",
    "voice_focus.json",
)
RESTORE_PENDING_FILE = "review_pilot_policy.restore-pending"
REVIEW_PILOT_FILE = "review_pilot_policy.json"
REVIEW_PILOT_ABSENT_FILE = "review_pilot_policy.absent"
REVIEW_PILOT_ABSENT_BYTES = b"review-pilot-policy-absent-v1\n"
MANIFEST_FILE = "SNAPSHOT_MANIFEST.json"
LABEL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
COUNT_TABLES = (
    "speech_segments",
    "review_events",
    "spot_checks",
    "model_versions",
    "import_jobs",
    "import_job_files",
)
MANIFEST_FIELDS = {
    "schema",
    "createdAtEpochSecs",
    "appGitSha",
    "sourceDataDir",
    "databaseEvidence",
    "files",
}
MANIFEST_FILE_FIELDS = {"path", "sizeBytes", "sha256"}
DATABASE_EVIDENCE_FIELDS = {
    "quickCheck",
    "integrityCheck",
    "foreignKeyViolationCount",
    "schemaVersion",
    "rowCounts",
}
WINDOWS_RESERVED_NAMES = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}


def state_absence_marker(name: str) -> str:
    return f"{name}.absent"


def state_absence_bytes(name: str) -> bytes:
    return f"cortex-snapshot-state-absent-v1:{name}\n".encode("ascii")


def default_data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if not appdata:
        raise RuntimeError("APPDATA is unavailable; pass --data-dir explicitly")
    return Path(appdata) / "cortex-speech"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_sha(repo_root: Path) -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=repo_root, text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def open_readonly(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True, timeout=30)


def validate_migration_history(actual: tuple[tuple[int, str], ...]) -> int:
    """Mirror Rust ``validate_applied_history`` for any external snapshot database."""

    try:
        canonical = source_migrations(DEFAULT_MIGRATIONS)
    except (OSError, ValueError) as error:
        raise RuntimeError(f"canonical migration history cannot be resolved: {error}") from error
    if not actual:
        raise RuntimeError("schema_migrations is missing or empty; external snapshot history cannot be proven")
    current = actual[-1][0]
    head = canonical[-1][0]
    if current > head:
        raise RuntimeError(f"database schema v{current} is newer than this source supports (v{head})")
    expected = tuple(row for row in canonical if row[0] <= current)
    if actual != expected:
        actual_by_version = dict(actual)
        expected_by_version = dict(expected)
        missing = sorted(set(expected_by_version) - set(actual_by_version))
        unknown = sorted(set(actual_by_version) - set(expected_by_version))
        mismatched = sorted(
            version
            for version in set(actual_by_version) & set(expected_by_version)
            if actual_by_version[version] != expected_by_version[version]
        )
        raise RuntimeError(
            "schema migration history is incomplete or altered: "
            f"missing={missing}, unknown={unknown}, descriptionMismatch={mismatched}"
        )
    return current


def db_evidence(path: Path) -> dict[str, object]:
    con = open_readonly(path)
    try:
        quick = [row[0] for row in con.execute("PRAGMA quick_check")]
        integrity = [row[0] for row in con.execute("PRAGMA integrity_check")]
        foreign_keys = con.execute("PRAGMA foreign_key_check").fetchall()
        counts: dict[str, int] = {}
        existing = {
            row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()
        }
        for table in COUNT_TABLES:
            if table in existing:
                counts[table] = int(con.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0])
        try:
            migration_history = tuple(
                (int(version), str(description))
                for version, description in con.execute(
                    "SELECT version, description FROM schema_migrations ORDER BY version"
                )
            )
        except sqlite3.Error as error:
            raise RuntimeError(f"schema migration history cannot be read: {error}") from error
        schema_version = validate_migration_history(migration_history)
        return {
            "quickCheck": quick,
            "integrityCheck": integrity,
            "foreignKeyViolationCount": len(foreign_keys),
            "schemaVersion": schema_version,
            "rowCounts": counts,
        }
    finally:
        con.close()


def online_backup(source: Path, destination: Path) -> None:
    src = open_readonly(source)
    dest = sqlite3.connect(destination)
    try:
        src.backup(dest, pages=4096, sleep=0.001)
        dest.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        dest.close()
        src.close()


def _reject_duplicate_object_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON object key")
        value[key] = item
    return value


def validate_review_pilot_policy(raw: bytes) -> dict[str, object]:
    """Match the Rust paid-pilot parser before preserving a policy as recovery state."""

    def reject_constant(value: str) -> object:
        raise ValueError(f"non-finite JSON number: {value}")

    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_object_keys,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, ValueError) as error:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} is invalid: {error}") from error
    if not isinstance(value, dict) or set(value) != {
        "schema_version",
        "after_review_event_id",
        "max_total_corpus_actions",
        "reviewers",
    }:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} fields do not match the controlled-pilot contract")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} schema_version must be exactly integer 1")
    after = value["after_review_event_id"]
    if type(after) is not int or not 0 <= after <= (2**63 - 1):
        raise RuntimeError(f"{REVIEW_PILOT_FILE} after_review_event_id must be a non-negative i64")
    if type(value["max_total_corpus_actions"]) is not int or value["max_total_corpus_actions"] != 20:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} must cap the pilot at exactly 20 corpus actions")
    reviewers = value["reviewers"]
    if not isinstance(reviewers, list) or len(reviewers) != 2:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} must contain exactly two reviewers")
    normalized_names: list[str] = []
    for reviewer in reviewers:
        if not isinstance(reviewer, dict) or set(reviewer) != {"name", "max_corpus_actions"}:
            raise RuntimeError(f"{REVIEW_PILOT_FILE} reviewer fields do not match the server contract")
        name = reviewer["name"]
        cap = reviewer["max_corpus_actions"]
        if not isinstance(name, str):
            raise RuntimeError(f"{REVIEW_PILOT_FILE} contains a non-string reviewer name")
        name = name.strip()
        if not name or len(name) > 40 or any(unicodedata.category(char) == "Cc" for char in name):
            raise RuntimeError(f"{REVIEW_PILOT_FILE} contains an invalid reviewer name")
        if type(cap) is not int or cap != 10:
            raise RuntimeError(f"{REVIEW_PILOT_FILE} must cap each reviewer at exactly 10 corpus actions")
        normalized_names.append("".join(char.lower() if "A" <= char <= "Z" else char for char in name))
    if normalized_names[0] == normalized_names[1]:
        raise RuntimeError(f"{REVIEW_PILOT_FILE} reviewer names must be distinct")
    return value


def capture_review_pilot_state(data_dir: Path, staging: Path) -> None:
    source = data_dir / REVIEW_PILOT_FILE
    try:
        raw = source.read_bytes()
    except FileNotFoundError:
        (staging / REVIEW_PILOT_ABSENT_FILE).write_bytes(REVIEW_PILOT_ABSENT_BYTES)
        return
    except OSError as error:
        raise RuntimeError(f"active {REVIEW_PILOT_FILE} cannot be read: {error}") from error
    validate_review_pilot_policy(raw)
    verify_controlled_pilot_focus(data_dir)
    (staging / REVIEW_PILOT_FILE).write_bytes(raw)


def capture_optional_state(data_dir: Path, staging: Path) -> None:
    for name in REQUIRED_STATE:
        source = data_dir / name
        try:
            metadata = source.lstat()
        except FileNotFoundError:
            (staging / state_absence_marker(name)).write_bytes(state_absence_bytes(name))
            continue
        except OSError as error:
            raise RuntimeError(f"recovery state cannot be inspected: {source}: {error}") from error
        if source.is_symlink() or not stat.S_ISREG(metadata.st_mode):
            raise RuntimeError(f"recovery state must be a regular, non-symlink file: {source}")
        shutil.copy2(source, staging / name)


def copy_state(data_dir: Path, staging: Path) -> None:
    capture_optional_state(data_dir, staging)
    capture_review_pilot_state(data_dir, staging)


def verify_optional_state(tree: Path, declared: set[str]) -> None:
    for name in REQUIRED_STATE:
        marker = state_absence_marker(name)
        present = name in declared
        absent = marker in declared
        if present == absent:
            raise RuntimeError(f"snapshot must contain exactly one of {name} or {marker}")
        if present:
            value = _load_strict_json(tree / name, name)
            if not isinstance(value, dict):
                raise RuntimeError(f"snapshot {name} must contain a JSON object")
        elif (tree / marker).read_bytes() != state_absence_bytes(name):
            raise RuntimeError(f"snapshot {marker} has invalid contents")


def verify_review_pilot_state(tree: Path) -> None:
    policy = tree / REVIEW_PILOT_FILE
    absent = tree / REVIEW_PILOT_ABSENT_FILE
    policy_present = os.path.lexists(policy)
    absent_present = os.path.lexists(absent)
    if policy_present == absent_present:
        raise RuntimeError(
            f"snapshot must contain exactly one of {REVIEW_PILOT_FILE} or {REVIEW_PILOT_ABSENT_FILE}"
        )
    if policy_present:
        if policy.is_symlink() or not policy.is_file():
            raise RuntimeError(f"snapshot {REVIEW_PILOT_FILE} is not a regular file")
        parsed = validate_review_pilot_policy(policy.read_bytes())
        verify_controlled_pilot_focus(tree)
        con = open_readonly(tree / DB_FILE)
        try:
            max_event_id = int(con.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0])
        finally:
            con.close()
        if parsed["after_review_event_id"] > max_event_id:
            raise RuntimeError(
                "snapshot pilot baseline is ahead of its database review-event maximum: "
                f"{parsed['after_review_event_id']} > {max_event_id}"
            )
    else:
        if absent.is_symlink() or not absent.is_file():
            raise RuntimeError(f"snapshot {REVIEW_PILOT_ABSENT_FILE} is not a regular file")
        if absent.read_bytes() != REVIEW_PILOT_ABSENT_BYTES:
            raise RuntimeError(f"snapshot {REVIEW_PILOT_ABSENT_FILE} has invalid contents")


def inventory(directory: Path) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for path in sorted(p for p in directory.iterdir() if p.is_file() and p.name != MANIFEST_FILE):
        rows.append({"path": path.name, "sizeBytes": path.stat().st_size, "sha256": sha256_file(path)})
    return rows


def write_manifest(
    staging: Path,
    *,
    created_at: int,
    source_data_dir: Path,
    source_evidence: dict[str, object],
    repo_sha: str,
) -> None:
    payload = {
        "schema": 2,
        "createdAtEpochSecs": created_at,
        "appGitSha": repo_sha,
        "sourceDataDir": str(source_data_dir.resolve()),
        "databaseEvidence": source_evidence,
        "files": inventory(staging),
    }
    (staging / MANIFEST_FILE).write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )


def _load_strict_json(path: Path, label: str) -> object:
    def reject_constant(value: str) -> object:
        raise ValueError(f"non-finite JSON number: {value}")

    try:
        return json.loads(
            path.read_bytes().decode("utf-8"),
            object_pairs_hook=_reject_duplicate_object_keys,
            parse_constant=reject_constant,
        )
    except (OSError, UnicodeDecodeError, ValueError, RecursionError) as error:
        raise RuntimeError(f"{label} is invalid or unreadable: {error}") from error


def _require_exact_object(value: object, fields: set[str], label: str) -> dict[str, object]:
    if not isinstance(value, dict):
        raise RuntimeError(f"{label} must be a JSON object")
    actual = set(value)
    if actual != fields:
        raise RuntimeError(
            f"{label} fields are invalid (missing={sorted(fields - actual)}, extra={sorted(actual - fields)})"
        )
    return value


def _require_nonnegative_int(value: object, label: str) -> int:
    if type(value) is not int or value < 0:
        raise RuntimeError(f"{label} must be a non-negative integer")
    return value


def _safe_manifest_name(value: object) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value in {".", ".."}
        or value.casefold() == MANIFEST_FILE.casefold()
    ):
        raise RuntimeError(f"snapshot manifest contains unsafe file path {value!r}")
    windows = PureWindowsPath(value)
    if (
        windows.drive
        or windows.root
        or len(windows.parts) != 1
        or "/" in value
        or "\\" in value
        or any(char in '<>:"|?*' or ord(char) < 32 for char in value)
        or value.endswith((" ", "."))
        or value.split(".", 1)[0].casefold() in WINDOWS_RESERVED_NAMES
    ):
        raise RuntimeError(f"snapshot manifest contains unsafe file path {value!r}")
    return value


def _require_regular_file(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise RuntimeError(f"{label} is missing or unreadable: {error}") from error
    if path.is_symlink() or not stat.S_ISREG(metadata.st_mode):
        raise RuntimeError(f"{label} must be a regular, non-symlink file")


def validate_manifest_tree(tree: Path) -> dict[str, object]:
    """Validate the exact schema-2 tree this writer emits, without trusting defaults or extras."""

    manifest_path = tree / MANIFEST_FILE
    _require_regular_file(manifest_path, MANIFEST_FILE)
    manifest = _require_exact_object(
        _load_strict_json(manifest_path, MANIFEST_FILE), MANIFEST_FIELDS, "schema-2 manifest"
    )
    if type(manifest["schema"]) is not int or manifest["schema"] != 2:
        raise RuntimeError("snapshot manifest schema must be exactly integer 2")
    _require_nonnegative_int(manifest["createdAtEpochSecs"], "manifest createdAtEpochSecs")
    if not isinstance(manifest["appGitSha"], str) or not manifest["appGitSha"]:
        raise RuntimeError("manifest appGitSha must be a non-empty string")
    if not isinstance(manifest["sourceDataDir"], str) or not manifest["sourceDataDir"]:
        raise RuntimeError("manifest sourceDataDir must be a non-empty string")

    evidence = _require_exact_object(
        manifest["databaseEvidence"], DATABASE_EVIDENCE_FIELDS, "manifest databaseEvidence"
    )
    for field in ("quickCheck", "integrityCheck"):
        rows = evidence[field]
        if not isinstance(rows, list) or not rows or any(not isinstance(row, str) for row in rows):
            raise RuntimeError(f"manifest databaseEvidence.{field} must be a non-empty string array")
    _require_nonnegative_int(
        evidence["foreignKeyViolationCount"], "manifest databaseEvidence.foreignKeyViolationCount"
    )
    _require_nonnegative_int(evidence["schemaVersion"], "manifest databaseEvidence.schemaVersion")
    counts = _require_exact_object(
        evidence["rowCounts"], set(COUNT_TABLES), "manifest databaseEvidence.rowCounts"
    )
    for table, count in counts.items():
        _require_nonnegative_int(count, f"manifest databaseEvidence.rowCounts.{table}")

    rows = manifest["files"]
    if not isinstance(rows, list):
        raise RuntimeError("snapshot manifest files must be an array")
    declared: dict[str, dict[str, object]] = {}
    folded: set[str] = set()
    for index, raw_row in enumerate(rows):
        row = _require_exact_object(raw_row, MANIFEST_FILE_FIELDS, f"snapshot manifest file row {index}")
        name = _safe_manifest_name(row["path"])
        folded_name = name.casefold()
        if folded_name in folded:
            raise RuntimeError(f"snapshot manifest contains duplicate file {name!r}")
        folded.add(folded_name)
        _require_nonnegative_int(row["sizeBytes"], f"snapshot manifest size for {name!r}")
        digest = row["sha256"]
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(char not in "0123456789abcdef" for char in digest)
        ):
            raise RuntimeError(f"snapshot manifest SHA-256 for {name!r} must be 64 lowercase hex digits")
        declared[name] = row

    actual: dict[str, Path] = {}
    actual_folded: set[str] = set()
    try:
        entries = list(tree.iterdir())
    except OSError as error:
        raise RuntimeError(f"snapshot tree is unreadable: {error}") from error
    for path in entries:
        if path.name == MANIFEST_FILE:
            continue
        name = _safe_manifest_name(path.name)
        _require_regular_file(path, f"snapshot file {name!r}")
        folded_name = name.casefold()
        if folded_name in actual_folded:
            raise RuntimeError(f"snapshot tree contains a case-colliding duplicate file {name!r}")
        actual_folded.add(folded_name)
        actual[name] = path
    missing = sorted(set(declared) - set(actual))
    unlisted = sorted(set(actual) - set(declared))
    if missing or unlisted:
        raise RuntimeError(f"snapshot manifest inventory is not exact (missing={missing}, unlisted={unlisted})")
    for name, row in declared.items():
        path = actual[name]
        if path.stat().st_size != row["sizeBytes"]:
            raise RuntimeError(f"snapshot file size does not match its manifest: {path}")
        if sha256_file(path) != row["sha256"]:
            raise RuntimeError(f"snapshot file hash does not match its manifest: {path}")

    required = {DB_FILE}
    missing_required = sorted(required - set(declared))
    if missing_required:
        raise RuntimeError(f"snapshot manifest is missing required recovery state: {missing_required}")
    verify_optional_state(tree, set(declared))
    policy_present = REVIEW_PILOT_FILE in declared
    absence_present = REVIEW_PILOT_ABSENT_FILE in declared
    if policy_present == absence_present:
        raise RuntimeError(
            f"snapshot must contain exactly one of {REVIEW_PILOT_FILE} or {REVIEW_PILOT_ABSENT_FILE}"
        )
    return manifest


def verify_tree(
    tree: Path,
    *,
    expected_evidence: dict[str, object],
    expected_foreign_keys: int,
) -> None:
    manifest = validate_manifest_tree(tree)
    if manifest["databaseEvidence"] != expected_evidence:
        raise RuntimeError("snapshot manifest databaseEvidence differs from the live source evidence")
    verify_review_pilot_state(tree)
    evidence = db_evidence(tree / DB_FILE)
    if evidence["quickCheck"] != ["ok"] or evidence["integrityCheck"] != ["ok"]:
        raise RuntimeError(f"snapshot database failed SQLite checks: {evidence}")
    if evidence["foreignKeyViolationCount"] != expected_foreign_keys:
        raise RuntimeError(
            "snapshot foreign-key count changed: "
            f"expected {expected_foreign_keys}, got {evidence['foreignKeyViolationCount']}"
        )
    if evidence != expected_evidence:
        raise RuntimeError(f"snapshot database evidence differs from live source: {evidence} != {expected_evidence}")


def unique_final(root: Path, label: str, epoch: int) -> Path:
    candidate = root / f"{label}_{epoch:010d}"
    while candidate.exists():
        epoch += 1
        candidate = root / f"{label}_{epoch:010d}"
    return candidate


def canonical_path(path: Path) -> Path:
    return Path(os.path.normcase(str(path.resolve(strict=False))))


def is_same_or_descendant(path: Path, directory: Path) -> bool:
    try:
        path.relative_to(directory)
        return True
    except ValueError:
        return False


def require_disjoint_offsite(local: Path, offsite_dir: Path) -> None:
    """Reject same-tree mirrors before creating, copying, renaming, or deleting anything."""
    source = canonical_path(local)
    destinations = (canonical_path(offsite_dir), canonical_path(offsite_dir / "snapshots" / "pinned"))
    for destination in destinations:
        if is_same_or_descendant(source, destination) or is_same_or_descendant(destination, source):
            raise RuntimeError("offsite snapshot destination overlaps the local snapshot source")


def _promote_snapshot_locked(
    data_dir: Path,
    *,
    label: str,
    expected_foreign_keys: int,
    repo_root: Path,
) -> tuple[Path, dict[str, object]]:
    pending = data_dir / RESTORE_PENDING_FILE
    if os.path.lexists(pending):
        raise RuntimeError(
            f"recovery snapshot refused while an interrupted restore is pending: {pending}"
        )
    source_db = data_dir / DB_FILE
    if not source_db.is_file():
        raise RuntimeError(f"live database is missing: {source_db}")
    evidence = db_evidence(source_db)
    if evidence["quickCheck"] != ["ok"] or evidence["integrityCheck"] != ["ok"]:
        raise RuntimeError(f"live database failed SQLite checks: {evidence}")
    if evidence["foreignKeyViolationCount"] != expected_foreign_keys:
        raise RuntimeError(
            "live foreign-key count does not match the operator's explicit expectation: "
            f"expected {expected_foreign_keys}, got {evidence['foreignKeyViolationCount']}"
        )

    epoch = int(time.time())
    root = data_dir / "snapshots" / "pinned"
    root.mkdir(parents=True, exist_ok=True)
    final = unique_final(root, label, epoch)
    staging = root / f".{final.name}.staging-{os.getpid()}"
    if staging.exists():
        raise RuntimeError(f"refusing to reuse snapshot staging directory: {staging}")
    staging.mkdir()
    try:
        online_backup(source_db, staging / DB_FILE)
        copy_state(data_dir, staging)
        write_manifest(
            staging,
            created_at=epoch,
            source_data_dir=data_dir,
            source_evidence=evidence,
            repo_sha=git_sha(repo_root),
        )
        verify_tree(staging, expected_evidence=evidence, expected_foreign_keys=expected_foreign_keys)
        staging.rename(final)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return final, evidence


def promote_snapshot(
    data_dir: Path,
    *,
    label: str,
    expected_foreign_keys: int,
    repo_root: Path,
) -> tuple[Path, dict[str, object]]:
    """Capture and promote while exclusively owning the app/importer lock.

    The lock spans source evidence, SQLite backup, configuration capture, self-verification, and
    final rename. A snapshot therefore cannot interleave with an official app/importer mutation.
    """

    with acquire_cortex_lock(data_dir):
        return _promote_snapshot_locked(
            data_dir,
            label=label,
            expected_foreign_keys=expected_foreign_keys,
            repo_root=repo_root,
        )


def mirror_offsite(
    local: Path,
    offsite_dir: Path,
    *,
    evidence: dict[str, object],
    expected_foreign_keys: int,
) -> Path:
    if not local.is_dir():
        raise RuntimeError(f"local snapshot is missing: {local}")
    require_disjoint_offsite(local, offsite_dir)
    root = offsite_dir / "snapshots" / "pinned"
    root.mkdir(parents=True, exist_ok=True)
    # Resolve again after creation so an existing junction/symlink cannot disguise an overlap.
    require_disjoint_offsite(local, offsite_dir)
    final = root / local.name
    if os.path.lexists(final):
        raise RuntimeError(f"offsite snapshot already exists: {final}")
    owned_parent: Path | None = None
    try:
        # The random parent is atomically and exclusively created by this invocation. Cleanup is
        # therefore limited to a tree we own; a predictable pre-existing path is never removed.
        owned_parent = Path(tempfile.mkdtemp(prefix=f".{local.name}.staging-", dir=root))
        staging = owned_parent / "tree"
        shutil.copytree(local, staging)
        verify_tree(staging, expected_evidence=evidence, expected_foreign_keys=expected_foreign_keys)
        staging.rename(final)
        try:
            owned_parent.rmdir()
        except OSError:
            shutil.rmtree(owned_parent, ignore_errors=True)
        owned_parent = None
    except Exception:
        if owned_parent is not None:
            shutil.rmtree(owned_parent, ignore_errors=True)
        raise
    return final


def offsite_from_settings(data_dir: Path) -> Path | None:
    settings_path = data_dir / "settings.json"
    if not settings_path.is_file():
        return None
    value = json.loads(settings_path.read_text(encoding="utf-8")).get("backup_second_dir")
    return Path(value) if isinstance(value, str) and value.strip() else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=None)
    parser.add_argument("--offsite-dir", type=Path, default=None)
    parser.add_argument("--no-offsite", action="store_true")
    parser.add_argument("--label", required=True)
    parser.add_argument("--expected-foreign-key-violations", type=int, default=0)
    args = parser.parse_args()
    if not LABEL_RE.fullmatch(args.label):
        parser.error("--label must be 1-64 ASCII letters, digits, '_' or '-', starting alphanumeric")
    if args.expected_foreign_key_violations < 0:
        parser.error("--expected-foreign-key-violations cannot be negative")

    data_dir = (args.data_dir or default_data_dir()).resolve()
    repo_root = Path(__file__).resolve().parents[2]
    local, evidence = promote_snapshot(
        data_dir,
        label=args.label,
        expected_foreign_keys=args.expected_foreign_key_violations,
        repo_root=repo_root,
    )
    print(f"LOCAL_SNAPSHOT={local}")
    offsite = None if args.no_offsite else (args.offsite_dir or offsite_from_settings(data_dir))
    if not args.no_offsite and offsite is None:
        raise RuntimeError("no offsite directory configured; pass --offsite-dir or --no-offsite explicitly")
    if offsite is not None:
        remote = mirror_offsite(
            local,
            offsite.resolve(),
            evidence=evidence,
            expected_foreign_keys=args.expected_foreign_key_violations,
        )
        print(f"OFFSITE_SNAPSHOT={remote}")
    print(json.dumps(evidence, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"SNAPSHOT FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
