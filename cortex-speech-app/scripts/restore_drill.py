#!/usr/bin/env python3
"""Fail-closed recovery drill for a Cortex snapshot.

The drill restores only into a disposable temporary profile. A production pass requires a strict,
complete ``SNAPSHOT_MANIFEST.json`` written by either the Rust snapshot writer (schema 1) or the
headless recovery writer (schema 2). Manifest-less historical trees are deliberately refused: they
can be inspected manually, but cannot prove a production recovery.

Usage:
    python scripts/restore_drill.py <snapshot-dir> [--expect-fail]

``--expect-fail`` is the negative-control mode. A drill that cannot reject an intentionally broken
tree proves nothing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sqlite3
import stat
import tempfile
import unicodedata
from dataclasses import dataclass
from pathlib import Path, PureWindowsPath
from typing import Any

from check_database_integrity import DEFAULT_MIGRATIONS, source_migrations
from pilot_focus_contract import verify_controlled_pilot_focus
from review_pilot_hidden_contract import (
    HIDDEN_KEYS_PER_REVIEWER,
    HIDDEN_TABLE as HIDDEN_KEY_TABLE,
    HIDDEN_TABLE_SQL,
    HIDDEN_TRIGGER_SQL,
    HIDDEN_KEY_SCHEMA_VERSION,
    PILOT_REVIEWERS,
    TOTAL_HIDDEN_KEYS,
    ReviewPilotPolicy as HiddenReviewPilotPolicy,
    normalized_sql,
    policy_sha256 as hidden_policy_sha256,
)

DB_FILE = "cortex-speech.db"
# Each state has a mandatory representation: the real JSON file or its exact absence marker.
REQUIRED_STATE = ("settings.json", "champion.json", "reviewer_dialects.json", "voice_focus.json")
REVIEW_PILOT_FILE = "review_pilot_policy.json"
REVIEW_PILOT_ABSENT_FILE = "review_pilot_policy.absent"
REVIEW_PILOT_ABSENT_BYTES = b"review-pilot-policy-absent-v1\n"
MANIFEST = "SNAPSHOT_MANIFEST.json"
HUMAN_TABLES = ("speech_segments", "review_events", "spot_checks", "model_versions")
BASE_EVIDENCE_TABLES = HUMAN_TABLES + ("import_jobs", "import_job_files")
FILE_ROW_FIELDS = {"path", "sizeBytes", "sha256"}
SCHEMA_FIELDS = {
    1: {"schema", "reviewPilotPolicyStateSchema", "createdAtEpochSecs", "appGitSha", "files"},
    2: {"schema", "createdAtEpochSecs", "appGitSha", "sourceDataDir", "databaseEvidence", "files"},
}
DATABASE_EVIDENCE_FIELDS = {
    "quickCheck",
    "integrityCheck",
    "foreignKeyViolationCount",
    "schemaVersion",
    "rowCounts",
}
PILOT_FIELDS = {"schema_version", "after_review_event_id", "max_total_corpus_actions", "reviewers"}
PILOT_REVIEWER_FIELDS = {"name", "max_corpus_actions"}
WINDOWS_RESERVED_NAMES = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}


def evidence_tables_for_schema(schema_version: int) -> tuple[str, ...]:
    """Schema-2 evidence emitted before v59 did not—and must not—claim the v59 table."""

    return (
        BASE_EVIDENCE_TABLES + (HIDDEN_KEY_TABLE,)
        if schema_version >= HIDDEN_KEY_SCHEMA_VERSION
        else BASE_EVIDENCE_TABLES
    )


def state_absence_marker(name: str) -> str:
    return f"{name}.absent"


def state_absence_bytes(name: str) -> bytes:
    return f"cortex-snapshot-state-absent-v1:{name}\n".encode("ascii")


class SnapshotValidationError(RuntimeError):
    """A snapshot contract violation that must fail the drill closed."""


@dataclass(frozen=True)
class ManifestContract:
    schema: int
    file_names: tuple[str, ...]
    database_evidence: dict[str, Any] | None
    pilot_policy: dict[str, Any] | None


@dataclass(frozen=True)
class DatabaseInspection:
    evidence: dict[str, Any]
    human_counts: dict[str, int]
    max_review_event_id: int | None
    champion_row: tuple[str, str] | None
    migration_history: tuple[tuple[int, str], ...]
    existing_tables: frozenset[str]
    hidden_key_rows: tuple[tuple[Any, Any, Any, Any], ...] | None
    hidden_key_table_sql: str | None
    hidden_key_triggers: tuple[tuple[str, str], ...]
    hidden_event_rows: tuple[tuple[Any, Any, Any, Any], ...]
    hidden_result_rows: tuple[tuple[Any, Any, Any], ...]


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _reject_duplicate_object_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object key {key!r}")
        value[key] = item
    return value


def _load_json_bytes(raw: bytes, label: str) -> Any:
    def reject_constant(value: str) -> Any:
        raise ValueError(f"non-finite JSON number {value}")

    try:
        return json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_object_keys,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, ValueError, RecursionError) as error:
        raise SnapshotValidationError(f"{label} is invalid JSON: {error}") from error


def _load_json_file(path: Path, label: str) -> Any:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise SnapshotValidationError(f"{label} is unreadable: {error}") from error
    return _load_json_bytes(raw, label)


def _require_exact_object(value: Any, fields: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SnapshotValidationError(f"{label} must be a JSON object")
    actual = set(value)
    if actual != fields:
        missing = sorted(fields - actual)
        extra = sorted(actual - fields)
        raise SnapshotValidationError(f"{label} fields are invalid (missing={missing}, extra={extra})")
    return value


def _require_nonnegative_int(value: Any, label: str) -> int:
    if type(value) is not int or value < 0:
        raise SnapshotValidationError(f"{label} must be a non-negative integer")
    return value


def _safe_single_component(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value in {".", ".."}
        or value.casefold() == MANIFEST.casefold()
    ):
        raise SnapshotValidationError(f"snapshot manifest contains unsafe file path {value!r}")
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
        raise SnapshotValidationError(f"snapshot manifest contains unsafe file path {value!r}")
    return value


def _regular_file(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise SnapshotValidationError(f"{label} is missing or unreadable: {error}") from error
    if path.is_symlink() or not stat.S_ISREG(metadata.st_mode):
        raise SnapshotValidationError(f"{label} must be a regular, non-symlink file")


def validate_review_pilot_policy(raw: bytes) -> dict[str, Any]:
    value = _require_exact_object(
        _load_json_bytes(raw, REVIEW_PILOT_FILE), PILOT_FIELDS, REVIEW_PILOT_FILE
    )
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} schema_version must be exactly integer 1")
    after = _require_nonnegative_int(value["after_review_event_id"], f"{REVIEW_PILOT_FILE} baseline")
    if after > 2**63 - 1:
        raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} baseline exceeds signed i64")
    if type(value["max_total_corpus_actions"]) is not int or value["max_total_corpus_actions"] != 20:
        raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} must cap the pilot at exactly 20 actions")
    reviewers = value["reviewers"]
    if not isinstance(reviewers, list) or len(reviewers) != 2:
        raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} must contain exactly two reviewers")
    normalized_names: list[str] = []
    for index, raw_reviewer in enumerate(reviewers):
        reviewer = _require_exact_object(
            raw_reviewer, PILOT_REVIEWER_FIELDS, f"{REVIEW_PILOT_FILE} reviewer {index}"
        )
        name = reviewer["name"]
        if not isinstance(name, str):
            raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} reviewer {index} has an invalid name")
        name = name.strip()
        if not name or len(name) > 40:
            raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} reviewer {index} has an invalid name")
        if any(unicodedata.category(char) == "Cc" for char in name):
            raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} reviewer {index} has an invalid name")
        if type(reviewer["max_corpus_actions"]) is not int or reviewer["max_corpus_actions"] != 10:
            raise SnapshotValidationError(
                f"{REVIEW_PILOT_FILE} reviewer {index} must be capped at exactly 10 actions"
            )
        normalized_names.append("".join(char.lower() if "A" <= char <= "Z" else char for char in name))
    if normalized_names[0] == normalized_names[1]:
        raise SnapshotValidationError(f"{REVIEW_PILOT_FILE} reviewer names must be distinct")
    if set(normalized_names) != {_ascii_lower(name) for name in PILOT_REVIEWERS}:
        raise SnapshotValidationError(
            f"{REVIEW_PILOT_FILE} must name exactly {' and '.join(PILOT_REVIEWERS)}"
        )
    return value


def _ascii_lower(value: str) -> str:
    return "".join(chr(ord(char) + 32) if "A" <= char <= "Z" else char for char in value)


def review_pilot_policy_sha256(policy: dict[str, Any]) -> str:
    """Mirror Rust ``ReviewPilotPolicy::policy_sha256`` exactly."""

    reviewers = {
        str(entry["name"]).strip(): int(entry["max_corpus_actions"])
        for entry in policy["reviewers"]
    }
    return hidden_policy_sha256(
        HiddenReviewPilotPolicy(
            after_review_event_id=int(policy["after_review_event_id"]),
            max_total_corpus_actions=int(policy["max_total_corpus_actions"]),
            reviewer_caps=reviewers,
        )
    )


def validate_hidden_key_policy_binding(
    inspection: DatabaseInspection,
    policy: dict[str, Any] | None,
) -> None:
    """Validate append-only v59 history and bind only the active pilot namespace."""

    schema_version = int(inspection.evidence["schemaVersion"])
    table_present = HIDDEN_KEY_TABLE in inspection.existing_tables
    if schema_version < HIDDEN_KEY_SCHEMA_VERSION:
        if table_present:
            raise SnapshotValidationError(
                f"{HIDDEN_KEY_TABLE} exists before its schema v{HIDDEN_KEY_SCHEMA_VERSION} migration"
            )
        if policy is not None:
            raise SnapshotValidationError(
                f"policy-bearing schema v{schema_version} snapshot predates durable hidden-key "
                f"authority v{HIDDEN_KEY_SCHEMA_VERSION}; it is archival only and not production-restorable"
            )
        return
    if not table_present or inspection.hidden_key_rows is None:
        raise SnapshotValidationError(
            f"schema v{schema_version} is missing required {HIDDEN_KEY_TABLE} authority"
        )
    if normalized_sql(inspection.hidden_key_table_sql or "") != normalized_sql(HIDDEN_TABLE_SQL):
        raise SnapshotValidationError(
            f"{HIDDEN_KEY_TABLE} does not exactly match the schema v59 authority contract"
        )
    actual_triggers = dict(inspection.hidden_key_triggers)
    missing_triggers = sorted(set(HIDDEN_TRIGGER_SQL) - set(actual_triggers))
    unexpected_triggers = sorted(set(actual_triggers) - set(HIDDEN_TRIGGER_SQL))
    mismatched_triggers = sorted(
        name
        for name in set(HIDDEN_TRIGGER_SQL) & set(actual_triggers)
        if normalized_sql(actual_triggers[name]) != normalized_sql(HIDDEN_TRIGGER_SQL[name])
    )
    if missing_triggers or unexpected_triggers or mismatched_triggers:
        raise SnapshotValidationError(
            f"{HIDDEN_KEY_TABLE} trigger contract is invalid "
            f"(missing={missing_triggers}, unexpected={unexpected_triggers}, "
            f"mismatched={mismatched_triggers})"
        )

    rows = inspection.hidden_key_rows
    expected_sha = review_pilot_policy_sha256(policy) if policy is not None else None
    expected_baseline = int(policy["after_review_event_id"]) if policy is not None else None
    authorized = (
        {_ascii_lower(str(entry["name"]).strip()) for entry in policy["reviewers"]}
        if policy is not None
        else set()
    )
    namespace_counts: dict[tuple[str, int], int] = {}
    reviewer_counts: dict[tuple[str, int, str], int] = {}
    active_grants: set[tuple[str, str]] = set()
    for policy_sha, baseline, reviewer, segment_id in rows:
        if (
            not isinstance(policy_sha, str)
            or len(policy_sha) != 64
            or any(char not in "0123456789abcdef" for char in policy_sha)
        ):
            raise SnapshotValidationError(
                f"{HIDDEN_KEY_TABLE} contains a non-canonical policy SHA-256"
            )
        if type(baseline) is not int or baseline < 0:
            raise SnapshotValidationError(
                f"{HIDDEN_KEY_TABLE} contains an invalid policy baseline"
            )
        if (
            not isinstance(reviewer, str)
            or reviewer != reviewer.strip()
            or not 1 <= len(reviewer) <= 40
        ):
            raise SnapshotValidationError(f"{HIDDEN_KEY_TABLE} contains an invalid reviewer")
        canonical_reviewer = _ascii_lower(reviewer)
        if (
            not isinstance(segment_id, str)
            or segment_id != segment_id.strip()
            or not 1 <= len(segment_id.encode("utf-8")) <= 256
            or not all(char.isalnum() or char in "_-." for char in segment_id)
        ):
            raise SnapshotValidationError(f"{HIDDEN_KEY_TABLE} contains an invalid segment id")

        namespace = (policy_sha, baseline)
        reviewer_namespace = (policy_sha, baseline, canonical_reviewer)
        namespace_counts[namespace] = namespace_counts.get(namespace, 0) + 1
        reviewer_counts[reviewer_namespace] = reviewer_counts.get(reviewer_namespace, 0) + 1

        if policy is not None:
            same_sha = policy_sha == expected_sha
            same_baseline = baseline == expected_baseline
            if same_sha != same_baseline:
                raise SnapshotValidationError(
                    f"{HIDDEN_KEY_TABLE} grant disagrees with the active policy SHA/baseline namespace"
                )
            if same_sha and canonical_reviewer not in authorized:
                raise SnapshotValidationError(
                    f"{HIDDEN_KEY_TABLE} contains unauthorized reviewer {reviewer!r} in the active namespace"
                )
            if same_sha:
                active_grants.add((canonical_reviewer, segment_id))

    reviewer_overages = sorted(
        key for key, count in reviewer_counts.items() if count > HIDDEN_KEYS_PER_REVIEWER
    )
    if reviewer_overages:
        raise SnapshotValidationError(
            f"{HIDDEN_KEY_TABLE} exceeds the {HIDDEN_KEYS_PER_REVIEWER}-grant reviewer namespace cap: "
            f"{reviewer_overages}"
        )
    namespace_overages = sorted(
        key for key, count in namespace_counts.items() if count > TOTAL_HIDDEN_KEYS
    )
    if namespace_overages:
        raise SnapshotValidationError(
            f"{HIDDEN_KEY_TABLE} exceeds the {TOTAL_HIDDEN_KEYS}-grant policy namespace cap: "
            f"{namespace_overages}"
        )

    if policy is not None:
        completed: set[tuple[str, str]] = set()
        for event_id, segment_id, reviewer, action in inspection.hidden_event_rows:
            if type(event_id) is not int or event_id <= expected_baseline:
                continue
            canonical_reviewer = _ascii_lower(str(reviewer).strip())
            if canonical_reviewer not in authorized:
                raise SnapshotValidationError(
                    f"post-baseline hidden event {event_id} has unauthorized reviewer {reviewer!r}"
                )
            if (
                not isinstance(segment_id, str)
                or segment_id != segment_id.strip()
                or not 1 <= len(segment_id.encode("utf-8")) <= 256
                or not all(char.isalnum() or char in "_-." for char in segment_id)
            ):
                raise SnapshotValidationError(
                    f"post-baseline hidden event {event_id} has an invalid segment id"
                )
            if action not in {"accept", "edit", "reject", "skip"}:
                raise SnapshotValidationError(
                    f"post-baseline hidden event {event_id} has invalid action {action!r}"
                )
            key = (canonical_reviewer, segment_id)
            if key not in active_grants:
                raise SnapshotValidationError(
                    f"post-baseline hidden event {event_id} has no durable active-policy grant"
                )
            if key in completed:
                raise SnapshotValidationError(
                    f"post-baseline hidden key {reviewer}/{segment_id} has multiple completion events"
                )
            completed.add(key)
            # The event and scored result are committed together. A recovery artifact that has only
            # one half would make runtime resolution and payment/QC reporting disagree.
            observed_actions = [
                str(row[2])
                for row in inspection.hidden_result_rows
                if str(row[0]) == segment_id
                and _ascii_lower(str(row[1]).strip()) == canonical_reviewer
            ]
            if observed_actions != [action]:
                raise SnapshotValidationError(
                    f"post-baseline hidden event {event_id} result mismatch: "
                    f"event={action!r}, results={observed_actions!r}"
                )


def _validate_database_evidence_shape(value: Any) -> dict[str, Any]:
    evidence = _require_exact_object(value, DATABASE_EVIDENCE_FIELDS, "schema-2 databaseEvidence")
    for field in ("quickCheck", "integrityCheck"):
        rows = evidence[field]
        if not isinstance(rows, list) or not rows or any(not isinstance(row, str) for row in rows):
            raise SnapshotValidationError(f"schema-2 databaseEvidence.{field} must be a non-empty string array")
    _require_nonnegative_int(
        evidence["foreignKeyViolationCount"], "schema-2 databaseEvidence.foreignKeyViolationCount"
    )
    schema_version = _require_nonnegative_int(
        evidence["schemaVersion"], "schema-2 databaseEvidence.schemaVersion"
    )
    counts = _require_exact_object(
        evidence["rowCounts"],
        set(evidence_tables_for_schema(schema_version)),
        "schema-2 databaseEvidence.rowCounts",
    )
    for table, count in counts.items():
        _require_nonnegative_int(count, f"schema-2 databaseEvidence.rowCounts.{table}")
    return evidence


def validate_migration_history(actual: tuple[tuple[int, str], ...]) -> int:
    """Mirror Rust ``validate_applied_history``: exact description-bound canonical prefix."""

    try:
        canonical = source_migrations(DEFAULT_MIGRATIONS)
    except (OSError, ValueError) as error:
        raise SnapshotValidationError(f"canonical migration history cannot be resolved: {error}") from error
    if not actual:
        raise SnapshotValidationError(
            "schema_migrations is missing or empty; external snapshot history cannot be proven"
        )
    current = actual[-1][0]
    head = canonical[-1][0]
    if current > head:
        raise SnapshotValidationError(
            f"restored schema v{current} is newer than this source supports (v{head})"
        )
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
        raise SnapshotValidationError(
            "schema migration history is incomplete or altered: "
            f"missing={missing}, unknown={unknown}, descriptionMismatch={mismatched}"
        )
    return current


def _actual_inventory(directory: Path) -> dict[str, Path]:
    inventory: dict[str, Path] = {}
    folded: set[str] = set()
    try:
        entries = list(directory.iterdir())
    except OSError as error:
        raise SnapshotValidationError(f"snapshot directory is unreadable: {error}") from error
    for path in entries:
        if path.name == MANIFEST:
            _regular_file(path, MANIFEST)
            continue
        name = _safe_single_component(path.name)
        _regular_file(path, f"snapshot file {name!r}")
        folded_name = name.casefold()
        if folded_name in folded:
            raise SnapshotValidationError(f"snapshot tree contains a case-colliding duplicate file {name!r}")
        folded.add(folded_name)
        inventory[name] = path
    return inventory


def validate_snapshot_manifest(directory: Path) -> ManifestContract:
    manifest_path = directory / MANIFEST
    _regular_file(manifest_path, MANIFEST)
    manifest_value = _load_json_file(manifest_path, MANIFEST)
    if not isinstance(manifest_value, dict):
        raise SnapshotValidationError(f"{MANIFEST} must be a JSON object")
    schema = manifest_value.get("schema")
    if type(schema) is not int or schema not in SCHEMA_FIELDS:
        raise SnapshotValidationError(f"{MANIFEST} schema must be exactly integer 1 or 2")
    manifest = _require_exact_object(manifest_value, SCHEMA_FIELDS[schema], f"schema-{schema} manifest")
    _require_nonnegative_int(manifest["createdAtEpochSecs"], "manifest createdAtEpochSecs")
    if not isinstance(manifest["appGitSha"], str) or not manifest["appGitSha"]:
        raise SnapshotValidationError("manifest appGitSha must be a non-empty string")
    if schema == 1:
        if type(manifest["reviewPilotPolicyStateSchema"]) is not int or manifest[
            "reviewPilotPolicyStateSchema"
        ] != 1:
            raise SnapshotValidationError("schema-1 reviewPilotPolicyStateSchema must be exactly integer 1")
        database_evidence = None
    else:
        if not isinstance(manifest["sourceDataDir"], str) or not manifest["sourceDataDir"]:
            raise SnapshotValidationError("schema-2 sourceDataDir must be a non-empty string")
        database_evidence = _validate_database_evidence_shape(manifest["databaseEvidence"])

    rows = manifest["files"]
    if not isinstance(rows, list):
        raise SnapshotValidationError("manifest files must be an array")
    declared: dict[str, dict[str, Any]] = {}
    folded: set[str] = set()
    for index, raw_row in enumerate(rows):
        row = _require_exact_object(raw_row, FILE_ROW_FIELDS, f"manifest file row {index}")
        name = _safe_single_component(row["path"])
        folded_name = name.casefold()
        if folded_name in folded:
            raise SnapshotValidationError(f"manifest contains duplicate file {name!r}")
        folded.add(folded_name)
        _require_nonnegative_int(row["sizeBytes"], f"manifest size for {name!r}")
        digest = row["sha256"]
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(char not in "0123456789abcdef" for char in digest)
        ):
            raise SnapshotValidationError(f"manifest SHA-256 for {name!r} must be 64 lowercase hex digits")
        declared[name] = row

    actual = _actual_inventory(directory)
    missing = sorted(set(declared) - set(actual))
    unlisted = sorted(set(actual) - set(declared))
    if missing or unlisted:
        raise SnapshotValidationError(
            f"manifest inventory is not exact (missing={missing}, unlisted={unlisted})"
        )
    for name, row in declared.items():
        path = actual[name]
        size = path.stat().st_size
        if size != row["sizeBytes"]:
            raise SnapshotValidationError(
                f"snapshot file {name!r} size mismatch: expected {row['sizeBytes']}, got {size}"
            )
        if sha256_of(path) != row["sha256"]:
            raise SnapshotValidationError(f"snapshot file {name!r} SHA-256 mismatch")

    required = {DB_FILE}
    absent_required = sorted(required - set(declared))
    if absent_required:
        raise SnapshotValidationError(f"manifest is incomplete: missing required files {absent_required}")
    for name in REQUIRED_STATE:
        marker = state_absence_marker(name)
        present = name in declared
        absent = marker in declared
        if present == absent:
            raise SnapshotValidationError(f"manifest must contain exactly one of {name} or {marker}")
        if absent and actual[marker].read_bytes() != state_absence_bytes(name):
            raise SnapshotValidationError(f"{marker} has invalid contents")
    policy_present = REVIEW_PILOT_FILE in declared
    absence_present = REVIEW_PILOT_ABSENT_FILE in declared
    if policy_present == absence_present:
        raise SnapshotValidationError(
            f"manifest must contain exactly one of {REVIEW_PILOT_FILE} or {REVIEW_PILOT_ABSENT_FILE}"
        )
    pilot_policy = None
    if policy_present:
        pilot_policy = validate_review_pilot_policy(actual[REVIEW_PILOT_FILE].read_bytes())
        try:
            verify_controlled_pilot_focus(directory)
        except RuntimeError as error:
            raise SnapshotValidationError(f"snapshot controlled-pilot focus is invalid: {error}") from error
    elif actual[REVIEW_PILOT_ABSENT_FILE].read_bytes() != REVIEW_PILOT_ABSENT_BYTES:
        raise SnapshotValidationError(f"{REVIEW_PILOT_ABSENT_FILE} has invalid contents")
    return ManifestContract(schema, tuple(declared), database_evidence, pilot_policy)


def inspect_database(path: Path) -> DatabaseInspection:
    try:
        # The manifest has already proved this is a closed, self-contained snapshot.  Immutable mode
        # keeps the recovery drill observational even when an older valid snapshot retains a WAL-mode
        # database header: inspection must not mint disposable ``-wal``/``-shm`` state.
        connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro&immutable=1", uri=True)
    except sqlite3.Error as error:
        raise SnapshotValidationError(f"restored database could not be opened read-only: {error}") from error
    try:
        existing = {
            str(row[0])
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()
        }
        quick = [str(row[0]) for row in connection.execute("PRAGMA quick_check").fetchall()]
        integrity = [str(row[0]) for row in connection.execute("PRAGMA integrity_check").fetchall()]
        foreign_keys = connection.execute("PRAGMA foreign_key_check").fetchall()
        migration_history = (
            tuple(
                (int(version), str(description))
                for version, description in connection.execute(
                    "SELECT version, description FROM schema_migrations ORDER BY version"
                )
            )
            if "schema_migrations" in existing
            else ()
        )
        schema_version = migration_history[-1][0] if migration_history else 0
        row_counts = {
            table: int(connection.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0])
            for table in evidence_tables_for_schema(schema_version)
            if table in existing
        }
        human_counts = {table: row_counts[table] for table in HUMAN_TABLES if table in row_counts}
        max_review_event_id = (
            int(connection.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0])
            if "review_events" in existing
            else None
        )
        champion_row = None
        if "model_versions" in existing:
            row = connection.execute(
                "SELECT id, checkpoint_sha256 FROM model_versions WHERE status='champion' ORDER BY id LIMIT 1"
            ).fetchone()
            if row is not None and isinstance(row[0], str) and isinstance(row[1], str):
                champion_row = (row[0], row[1])
        hidden_key_rows = None
        hidden_key_table_sql = None
        hidden_key_triggers: tuple[tuple[str, str], ...] = ()
        hidden_event_rows: tuple[tuple[Any, Any, Any, Any], ...] = ()
        hidden_result_rows: tuple[tuple[Any, Any, Any], ...] = ()
        if HIDDEN_KEY_TABLE in existing:
            table_row = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
                (HIDDEN_KEY_TABLE,),
            ).fetchone()
            hidden_key_table_sql = str(table_row[0] or "") if table_row is not None else ""
            hidden_key_rows = tuple(
                connection.execute(
                    f'SELECT policy_sha256, after_review_event_id, reviewer, segment_id '
                    f'FROM "{HIDDEN_KEY_TABLE}" '
                    f'ORDER BY policy_sha256, after_review_event_id, reviewer, segment_id'
                )
            )
            hidden_key_triggers = tuple(
                (str(row[0]), str(row[1] or ""))
                for row in connection.execute(
                    "SELECT name, sql FROM sqlite_master WHERE type='trigger' AND tbl_name=? ORDER BY name",
                    (HIDDEN_KEY_TABLE,),
                )
            )
        if "review_events" in existing:
            hidden_event_rows = tuple(
                connection.execute(
                    """SELECT id, segment_id, reviewer, action FROM review_events
                         WHERE source = 'couch_spot_check' ORDER BY id"""
                )
            )
        if "spot_checks" in existing:
            hidden_result_rows = tuple(
                connection.execute(
                    "SELECT segment_id, reviewer, action FROM spot_checks ORDER BY segment_id, reviewer"
                )
            )
        evidence = {
            "quickCheck": quick,
            "integrityCheck": integrity,
            "foreignKeyViolationCount": len(foreign_keys),
            "schemaVersion": schema_version,
            "rowCounts": row_counts,
        }
        return DatabaseInspection(
            evidence,
            human_counts,
            max_review_event_id,
            champion_row,
            migration_history,
            frozenset(existing),
            hidden_key_rows,
            hidden_key_table_sql,
            hidden_key_triggers,
            hidden_event_rows,
            hidden_result_rows,
        )
    except (sqlite3.Error, TypeError, ValueError) as error:
        raise SnapshotValidationError(f"restored database inspection failed: {error}") from error
    finally:
        connection.close()


def _load_recovery_state_objects(
    profile: Path, contract: ManifestContract
) -> dict[str, dict[str, Any] | None]:
    parsed: dict[str, dict[str, Any] | None] = {}
    for name in REQUIRED_STATE:
        if name not in contract.file_names:
            parsed[name] = None
            continue
        value = _load_json_file(profile / name, name)
        if not isinstance(value, dict):
            raise SnapshotValidationError(f"{name} must contain a JSON object")
        parsed[name] = value
    return parsed


def _copy_validated_snapshot(snapshot: Path, profile: Path, contract: ManifestContract) -> ManifestContract:
    for name in (*contract.file_names, MANIFEST):
        try:
            shutil.copyfile(snapshot / name, profile / name)
        except OSError as error:
            raise SnapshotValidationError(f"could not restore validated snapshot file {name!r}: {error}") from error
    # Prove the copied disposable tree—not merely the source—still matches after the copy boundary.
    return validate_snapshot_manifest(profile)


def drill(snapshot: Path) -> list[str]:
    problems: list[str] = []
    with tempfile.TemporaryDirectory(prefix="cortex-restore-drill-") as raw:
        profile = Path(raw) / "cortex-speech"
        profile.mkdir(parents=True)
        try:
            source_contract = validate_snapshot_manifest(snapshot)
            contract = _copy_validated_snapshot(snapshot, profile, source_contract)
            state = _load_recovery_state_objects(profile, contract)
            inspection = inspect_database(profile / DB_FILE)
            validate_migration_history(inspection.migration_history)
            validate_hidden_key_policy_binding(inspection, contract.pilot_policy)
        except (SnapshotValidationError, OSError) as error:
            return [str(error)]

        evidence = inspection.evidence
        if evidence["quickCheck"] != ["ok"]:
            problems.append(f"restored DB failed quick_check: {evidence['quickCheck']}")
        if evidence["integrityCheck"] != ["ok"]:
            problems.append(f"restored DB failed integrity_check: {evidence['integrityCheck']}")
        if evidence["foreignKeyViolationCount"] != 0:
            problems.append(
                f"restored DB has {evidence['foreignKeyViolationCount']} foreign-key violation(s)"
            )
        if evidence["schemaVersion"] <= 0:
            problems.append("restored DB has no positive schema migration version")
        missing_tables = sorted(set(HUMAN_TABLES) - set(inspection.human_counts))
        for table in missing_tables:
            problems.append(f"restored DB has no {table} table")
        if inspection.human_counts.get("speech_segments", 0) == 0:
            problems.append("restored DB holds zero segments — this is not a usable library")

        if contract.database_evidence is not None and contract.database_evidence != evidence:
            problems.append(
                "schema-2 databaseEvidence does not exactly match the restored database: "
                f"manifest={contract.database_evidence!r}, restored={evidence!r}"
            )
        if contract.pilot_policy is not None:
            baseline = contract.pilot_policy["after_review_event_id"]
            maximum = inspection.max_review_event_id
            if maximum is None:
                problems.append("restored DB has no review_events table for the paid-pilot baseline")
            elif baseline > maximum:
                problems.append(
                    "review pilot baseline is ahead of the restored DB review-event maximum: "
                    f"{baseline} > {maximum}"
                )

        champion_row = inspection.champion_row
        champion_state = state["champion.json"]
        if champion_state is None:
            if champion_row is None:
                problems.append("restored state has NO champion in either the registry or the pointer")
        else:
            champions = champion_state.get("champions")
            entry = champions.get("omniasr-7b") if isinstance(champions, dict) else None
            if champion_row and isinstance(entry, dict):
                if entry.get("modelVersionId") != champion_row[0]:
                    problems.append(
                        f"champion.json names {entry.get('modelVersionId')!r} but the registry champion is "
                        f"{champion_row[0]!r}"
                    )
                if entry.get("deploymentSha256") != champion_row[1]:
                    problems.append("champion.json deployment hash disagrees with the registry")
            elif champion_row and not isinstance(entry, dict):
                problems.append("registry has a champion but restored champion.json names none")
            elif isinstance(entry, dict) and not champion_row:
                problems.append("champion.json names a champion the restored registry does not hold")
            else:
                problems.append("restored state has NO champion in either the registry or the pointer")

        print(f"  manifest schema: {contract.schema}")
        print(f"  schema version : {evidence['schemaVersion']}")
        print(f"  row counts     : {inspection.human_counts}")
        if champion_row:
            print(f"  champion       : {champion_row[0]}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("--expect-fail", action="store_true", help="assert this tree is NOT restorable")
    args = parser.parse_args()
    if not args.snapshot.is_dir():
        raise SystemExit(f"not a snapshot directory: {args.snapshot}")

    print(f"RESTORE DRILL: {args.snapshot}")
    problems = drill(args.snapshot)
    for problem in problems:
        print(f"  - {problem}")

    if args.expect_fail:
        if problems:
            print(f"RESTORE DRILL: correctly REFUSED an unrestorable tree ({len(problems)} problem(s))")
            return 0
        print("RESTORE DRILL: FAILED — an incomplete tree passed, so this drill proves nothing")
        return 1
    if problems:
        print(f"RESTORE DRILL: FAILED — {len(problems)} problem(s)")
        return 1
    print("RESTORE DRILL: PASS — the library was fully recovered from this snapshot alone")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
