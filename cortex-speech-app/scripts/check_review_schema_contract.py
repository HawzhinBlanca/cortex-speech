#!/usr/bin/env python3
"""Fail closed when the live review SQLite schema differs from migrations 57/60-65.

The migration number alone is not proof that the safety objects still have their intended
definitions: SQLite permits a trigger or view to be dropped and recreated under the same name.
This gate compares every table, index, trigger, and view created by the compensation/effect and
private-production pool migrations with the canonical final SQL compiled from this checkout.  It
also checks every column those migrations add to an older table and rejects extra triggers attached
to the protected tables.  The four canonical speech-segment triggers that predate v57 are included
explicitly so ordinary FTS and revision maintenance is not misreported as hostile schema drift.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


REQUIRED_SCHEMA = 65
CONTRACT_MIGRATIONS = (57, 60, 61, 62, 63, 64, 65)
MIGRATIONS_SOURCE = Path(__file__).resolve().parents[1] / "src-tauri" / "src" / "migrations" / "mod.rs"

BASE_SPEECH_SEGMENT_TRIGGER_SQL = """
CREATE TRIGGER segments_ai AFTER INSERT ON speech_segments BEGIN
    INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
    VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
END;
CREATE TRIGGER segments_ad AFTER DELETE ON speech_segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
    VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
END;
"""


@dataclass(frozen=True)
class SchemaObject:
    object_type: str
    name: str
    sql: str


REQUIRED_ADDED_COLUMNS: dict[str, dict[str, tuple[str, int, str | None]]] = {
    "review_events": {
        "compensation_action": ("TEXT", 0, None),
        "operation_id": ("TEXT", 0, None),
        "operation_payload_hash": ("TEXT", 0, None),
        "app_git_sha": ("TEXT", 0, None),
        "playback_guard_version": ("TEXT", 0, None),
        "requested_action": ("TEXT", 0, None),
        "requested_transcript": ("TEXT", 0, None),
    },
    "agent_examples": {"effect_event_id": ("INTEGER", 0, None)},
    "corrections": {"effect_event_id": ("INTEGER", 0, None)},
    "correction_memory": {"legacy_seed": ("INTEGER", 1, "1")},
    "playback_receipts": {
        "source_start_ms": ("INTEGER", 0, None),
        "source_end_ms": ("INTEGER", 0, None),
    },
}

REQUIRED_ADDED_FOREIGN_KEYS = {
    ("agent_examples", "effect_event_id", "human_decision_effect_events", "id"),
    ("corrections", "effect_event_id", "human_decision_effect_events", "id"),
}


def _default_db() -> Path:
    appdata = os.environ.get("APPDATA")
    if not appdata:
        raise RuntimeError("APPDATA is not set; pass --db explicitly")
    return Path(appdata) / "cortex-speech" / "cortex-speech.db"


def _rust_string_value(block: str, marker: str = 'up_sql: "') -> str:
    start = block.find(marker)
    if start < 0:
        raise ValueError(f"migration block lacks {marker!r}")
    start += len(marker)
    chars: list[str] = []
    index = start
    while index < len(block):
        char = block[index]
        if char == '"':
            return "".join(chars)
        if char != "\\":
            chars.append(char)
            index += 1
            continue
        index += 1
        if index >= len(block):
            raise ValueError("unterminated Rust string escape")
        escaped = block[index]
        replacements = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", '"': '"'}
        if escaped not in replacements:
            raise ValueError(f"unsupported Rust string escape \\{escaped}")
        chars.append(replacements[escaped])
        index += 1
    raise ValueError("unterminated Rust migration SQL string")


def migration_up_sql(source: str, version: int) -> str:
    header = re.search(rf"\bMigration\s*\{{\s*version:\s*{version},", source)
    if header is None:
        raise ValueError(f"migration {version} is missing from {MIGRATIONS_SOURCE}")
    next_header = re.search(r"\n\s*Migration\s*\{", source[header.end() :])
    end = len(source) if next_header is None else header.end() + next_header.start()
    return _rust_string_value(source[header.start() : end])


def _without_line_comments(sql: str) -> str:
    cleaned: list[str] = []
    for line in sql.splitlines():
        in_quote = False
        index = 0
        while index + 1 < len(line):
            if line[index] == "'":
                if in_quote and index + 1 < len(line) and line[index + 1] == "'":
                    index += 2
                    continue
                in_quote = not in_quote
            if not in_quote and line[index : index + 2] == "--":
                line = line[:index]
                break
            index += 1
        cleaned.append(line)
    return "\n".join(cleaned)


def split_sql_statements(sql: str) -> list[str]:
    """Split migration SQL while retaining semicolons inside CREATE TRIGGER bodies."""
    sql = _without_line_comments(sql)
    statements: list[str] = []
    current: list[str] = []
    in_quote = False
    index = 0
    while index < len(sql):
        char = sql[index]
        current.append(char)
        if char == "'":
            if in_quote and index + 1 < len(sql) and sql[index + 1] == "'":
                current.append(sql[index + 1])
                index += 2
                continue
            in_quote = not in_quote
        if char == ";" and not in_quote:
            candidate = "".join(current).strip()
            is_trigger = bool(re.match(r"(?is)^CREATE\s+TRIGGER\b", candidate))
            if not is_trigger or re.search(r"(?is)\bEND\s*;\s*$", candidate):
                statements.append(candidate)
                current = []
        index += 1
    tail = "".join(current).strip()
    if tail:
        statements.append(tail)
    return statements


def normalize_schema_sql(sql: str) -> str:
    return " ".join(sql.strip().removesuffix(";").split()).casefold()


def created_schema_objects(sql: str) -> list[SchemaObject]:
    objects: list[SchemaObject] = []
    pattern = re.compile(
        r"(?is)^CREATE\s+(?:UNIQUE\s+)?(TABLE|INDEX|TRIGGER|VIEW)\s+"
        r"(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_]*)\b"
    )
    for statement in split_sql_statements(sql):
        match = pattern.match(statement)
        if match is None:
            continue
        objects.append(
            SchemaObject(match.group(1).casefold(), match.group(2), normalize_schema_sql(statement))
        )
    return objects


def load_contract_objects(source_path: Path = MIGRATIONS_SOURCE) -> tuple[dict[tuple[str, str], SchemaObject], str]:
    source = source_path.read_text(encoding="utf-8")
    objects: dict[tuple[str, str], SchemaObject] = {}
    contract_sql: list[str] = []
    # v60+ protects speech_segments, which legitimately already has two base FTS triggers and the
    # v53 narrowed FTS-update + monotonic-revision triggers. Bind their exact SQL first, in migration
    # order; otherwise the "no extra trigger" check rejects every honest production database.
    supporting_sql = [BASE_SPEECH_SEGMENT_TRIGGER_SQL, migration_up_sql(source, 53)]
    for sql in supporting_sql:
        contract_sql.append(sql)
        for item in created_schema_objects(sql):
            if item.object_type != "trigger" or item.name not in {
                "segments_ai",
                "segments_ad",
                "segments_au",
                "speech_segments_review_revision",
            }:
                continue
            key = (item.object_type, item.name)
            if key in objects:
                raise ValueError(f"duplicate schema-contract object {key}")
            objects[key] = item

    # Replay CREATE/DROP effects in version order so a later safety migration can deliberately
    # replace an earlier object.  Schema 65 does exactly this for the v64 excluded-duplicate guard:
    # comparing the first CREATE would certify stale SQL or reject the valid replacement.
    drop_pattern = re.compile(
        r"(?is)^DROP\s+(TABLE|INDEX|TRIGGER|VIEW)\s+(?:IF\s+EXISTS\s+)?"
        r"([A-Za-z_][A-Za-z0-9_]*)\b"
    )
    for version in CONTRACT_MIGRATIONS:
        up_sql = migration_up_sql(source, version)
        contract_sql.append(up_sql)
        for statement in split_sql_statements(up_sql):
            drop = drop_pattern.match(statement)
            if drop is not None:
                objects.pop((drop.group(1).casefold(), drop.group(2)), None)
                continue
            for item in created_schema_objects(statement):
                objects[(item.object_type, item.name)] = item
    digest = hashlib.sha256("\n".join(contract_sql).encode("utf-8")).hexdigest()
    return objects, digest


def compare_schema_objects(
    connection: sqlite3.Connection,
    expected: dict[tuple[str, str], SchemaObject],
) -> tuple[list[str], set[str]]:
    errors: list[str] = []
    protected_trigger_tables: set[str] = set()
    for (object_type, name), item in sorted(expected.items()):
        row = connection.execute(
            "SELECT type, tbl_name, sql FROM sqlite_master WHERE type=? AND name=?",
            (object_type, name),
        ).fetchone()
        if row is None:
            errors.append(f"missing schema contract object {object_type}:{name}")
            continue
        observed = normalize_schema_sql(str(row[2] or ""))
        if observed != item.sql:
            errors.append(f"schema contract SQL mismatch for {object_type}:{name}")
        if object_type == "trigger":
            protected_trigger_tables.add(str(row[1]))

    expected_trigger_names = {name for object_type, name in expected if object_type == "trigger"}
    if protected_trigger_tables:
        placeholders = ",".join("?" for _ in protected_trigger_tables)
        rows = connection.execute(
            f"SELECT name, tbl_name FROM sqlite_master WHERE type='trigger' AND tbl_name IN ({placeholders})",
            tuple(sorted(protected_trigger_tables)),
        ).fetchall()
        for name, table in rows:
            if str(name) not in expected_trigger_names:
                errors.append(f"unexpected trigger {name} on protected table {table}")
    return errors, protected_trigger_tables


def audit_added_columns(connection: sqlite3.Connection) -> list[str]:
    errors: list[str] = []
    for table, expected in REQUIRED_ADDED_COLUMNS.items():
        rows = {str(row[1]): row for row in connection.execute(f"PRAGMA table_xinfo('{table}')")}
        for column, (expected_type, expected_not_null, expected_default) in expected.items():
            row = rows.get(column)
            if row is None:
                errors.append(f"missing schema contract column {table}.{column}")
                continue
            observed = (str(row[2]).upper(), int(row[3]), None if row[4] is None else str(row[4]))
            wanted = (expected_type, expected_not_null, expected_default)
            if observed != wanted:
                errors.append(
                    f"schema contract column mismatch for {table}.{column}: "
                    f"observed={observed}, expected={wanted}"
                )

    observed_fks: set[tuple[str, str, str, str]] = set()
    for table in {item[0] for item in REQUIRED_ADDED_FOREIGN_KEYS}:
        for row in connection.execute(f"PRAGMA foreign_key_list('{table}')"):
            observed_fks.add((table, str(row[3]), str(row[2]), str(row[4])))
    for foreign_key in sorted(REQUIRED_ADDED_FOREIGN_KEYS - observed_fks):
        errors.append("missing schema contract foreign key " + ".".join(foreign_key))
    return errors


def _connect_read_only(path: Path) -> sqlite3.Connection:
    connection = sqlite3.connect(f"file:{path.resolve().as_posix()}?mode=ro", uri=True)
    connection.execute("PRAGMA query_only = ON")
    connection.execute("BEGIN")
    return connection


def audit(db_path: Path, source_path: Path = MIGRATIONS_SOURCE) -> dict[str, object]:
    evidence: dict[str, object] = {
        "database": str(db_path.resolve()),
        "migrationSource": str(source_path.resolve()),
        "requiredSchema": REQUIRED_SCHEMA,
    }
    if not db_path.is_file():
        return {**evidence, "ok": False, "errors": [f"database not found: {db_path}"]}
    if not source_path.is_file():
        return {**evidence, "ok": False, "errors": [f"migration source not found: {source_path}"]}
    try:
        expected, digest = load_contract_objects(source_path)
    except (OSError, ValueError) as error:
        return {**evidence, "ok": False, "errors": [f"cannot load schema contract: {error}"]}
    evidence["contractMigrations"] = list(CONTRACT_MIGRATIONS)
    evidence["contractObjects"] = len(expected)
    evidence["contractSqlSha256"] = digest

    try:
        connection = _connect_read_only(db_path)
    except sqlite3.Error as error:
        return {**evidence, "ok": False, "errors": [f"cannot open database read-only: {error}"]}
    errors: list[str] = []
    try:
        try:
            schema = int(
                connection.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
            )
        except sqlite3.Error:
            schema = 0
        evidence["schemaVersion"] = schema
        if schema != REQUIRED_SCHEMA:
            errors.append(f"schema {schema} is not exact required schema {REQUIRED_SCHEMA}")
        else:
            object_errors, protected_tables = compare_schema_objects(connection, expected)
            errors.extend(object_errors)
            errors.extend(audit_added_columns(connection))
            evidence["protectedTriggerTables"] = sorted(protected_tables)
    except sqlite3.Error as error:
        errors.append(f"schema contract query failed: {error}")
    finally:
        connection.rollback()
        connection.close()
    return {**evidence, "ok": not errors, "errors": errors}


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, help="SQLite database (default: live Cortex database)")
    parser.add_argument("--migration-source", type=Path, default=MIGRATIONS_SOURCE)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        db_path = args.db or _default_db()
        report = audit(db_path, args.migration_source)
    except (RuntimeError, OSError) as error:
        report = {"ok": False, "errors": [str(error)]}
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
