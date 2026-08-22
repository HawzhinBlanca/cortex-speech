#!/usr/bin/env python3
"""Fail-closed read-only integrity gate for the live Cortex SQLite database.

This is intentionally broader than feature-specific readiness gates.  A payment gate can prove its
own policy and ledger tables while an unrelated orphan still leaves the database unrecoverable.  A
release is therefore red unless SQLite's quick check, full integrity check, and whole-database
foreign-key check all pass on the exact live file.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
from collections import Counter
from pathlib import Path


DEFAULT_MIGRATIONS = Path(__file__).resolve().parent.parent / "src-tauri" / "src" / "migrations" / "mod.rs"
V58_PRODUCTION_ARCHIVE_ROWS = 2_104
V58_PRODUCTION_ID_SHA256 = "b4d84377b75f493383a8acbb63bea39482597f95060c32cf88eda6011fa0aec9"
V58_PRODUCTION_FULL_TUPLE_SHA256 = "5776c4a205e843bc7d7550242b1542a3640427089a2af4876744667db24cb2e0"
V58_ARCHIVE_TABLES = {
    "orphan_segment_hypotheses_archive_v58",
    "orphan_loop0_shadow_log_archive_v58",
}
V58_IMMUTABLE_TRIGGERS = {
    "orphan_segment_hypotheses_archive_v58_immutable_insert": (
        "orphan_segment_hypotheses_archive_v58",
        "insert",
    ),
    "orphan_segment_hypotheses_archive_v58_immutable_update": (
        "orphan_segment_hypotheses_archive_v58",
        "update",
    ),
    "orphan_segment_hypotheses_archive_v58_immutable_delete": (
        "orphan_segment_hypotheses_archive_v58",
        "delete",
    ),
    "orphan_loop0_shadow_log_archive_v58_immutable_insert": (
        "orphan_loop0_shadow_log_archive_v58",
        "insert",
    ),
    "orphan_loop0_shadow_log_archive_v58_immutable_update": (
        "orphan_loop0_shadow_log_archive_v58",
        "update",
    ),
    "orphan_loop0_shadow_log_archive_v58_immutable_delete": (
        "orphan_loop0_shadow_log_archive_v58",
        "delete",
    ),
}


def default_db_path() -> Path:
    # This is a production release gate, not a general database inspector.  Inherited environment
    # variables are not authority to redirect it to a clean fixture while the real AppData database is
    # broken.  Tests and deliberate diagnostics must opt in visibly with the CLI's explicit ``--db``.
    appdata = os.environ.get("APPDATA")
    base = Path(appdata) if appdata else Path.home() / ".local" / "share"
    return base / "cortex-speech" / "cortex-speech.db"


def source_migrations(migrations_path: Path) -> list[tuple[int, str]]:
    text = migrations_path.read_text(encoding="utf-8")
    migrations = [
        (int(version), description)
        for version, description in re.findall(
            r'Migration\s*\{\s*version:\s*(\d+)\s*,\s*description:\s*"([^"]*)"', text
        )
    ]
    if not migrations:
        raise ValueError("no migration versions were found")
    versions = [version for version, _description in migrations]
    if len(versions) != len(set(versions)):
        raise ValueError("migration versions are duplicated")
    ordered = sorted(migrations)
    if versions != [version for version, _description in ordered]:
        raise ValueError("source migrations are not in strictly increasing version order")
    if versions != list(range(1, max(versions) + 1)):
        raise ValueError("source migration history is not contiguous from version 1")
    return ordered


def latest_source_schema(migrations_path: Path) -> int:
    return source_migrations(migrations_path)[-1][0]


def _normalized_sql(value: str) -> str:
    return " ".join(value.strip().rstrip(";").lower().split())


def _audit_v58_evidence(
    connection: sqlite3.Connection,
    result: dict[str, object],
    errors: list[str],
    require_production_repair: bool,
    expected_rows: int,
    expected_digest: str,
    expected_full_tuple_digest: str,
) -> None:
    objects = {
        str(name): (str(kind), str(table_name), str(sql or ""))
        for kind, name, table_name, sql in connection.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema WHERE name LIKE '%archive_v58%'"
        )
    }
    missing_tables = sorted(V58_ARCHIVE_TABLES - set(objects))
    missing_triggers = sorted(set(V58_IMMUTABLE_TRIGGERS) - set(objects))
    if missing_tables:
        errors.append(f"v58 evidence archive table(s) missing: {missing_tables}")
    if missing_triggers:
        errors.append(f"v58 immutable archive trigger(s) missing: {missing_triggers}")
    for name, (table_name, operation) in V58_IMMUTABLE_TRIGGERS.items():
        if name not in objects:
            continue
        kind, actual_table, sql = objects[name]
        expected_sql = _normalized_sql(
            f"CREATE TRIGGER {name} BEFORE {operation.upper()} ON {table_name} "
            "BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END"
        )
        if kind != "trigger" or actual_table != table_name or _normalized_sql(sql) != expected_sql:
            errors.append(f"v58 archive trigger {name} does not match its immutable contract")
    if missing_tables:
        return

    hypothesis_rows = list(
        connection.execute(
            "SELECT original_rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id, "
            "source_table, archive_reason, archive_migration_version, archived_at "
            "FROM orphan_segment_hypotheses_archive_v58 ORDER BY segment_id"
        )
    )
    loop0_rows = list(
        connection.execute(
            "SELECT id, segment_id, memory_fired, created_at, source_table, archive_reason, "
            "archive_migration_version, archived_at "
            "FROM orphan_loop0_shadow_log_archive_v58 ORDER BY segment_id"
        )
    )
    hypothesis_ids = [str(row[1]) for row in hypothesis_rows]
    loop0_ids = [str(row[1]) for row in loop0_rows]
    digest = hashlib.sha256("".join(f"{segment_id}\n" for segment_id in hypothesis_ids).encode("utf-8")).hexdigest()
    full_tuple_hasher = hashlib.sha256()
    loop0_by_segment = {str(row[1]): row for row in loop0_rows}
    for hypothesis in hypothesis_rows:
        loop0 = loop0_by_segment.get(str(hypothesis[1]))
        if loop0 is None:
            continue
        source_pair = [list(hypothesis[:7]), list(loop0[:4])]
        full_tuple_hasher.update(
            (json.dumps(source_pair, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        )
    full_tuple_digest = full_tuple_hasher.hexdigest()
    result.update(
        {
            "v58HypothesisArchiveRows": len(hypothesis_rows),
            "v58Loop0ArchiveRows": len(loop0_rows),
            "v58ArchiveIdSha256": digest,
            "v58ArchiveFullTupleSha256": full_tuple_digest,
            "v58ImmutableTriggers": len(set(V58_IMMUTABLE_TRIGGERS) - set(missing_triggers)),
        }
    )

    if not hypothesis_rows and not loop0_rows:
        if require_production_repair:
            errors.append(f"production v58 repair evidence is empty; expected {expected_rows}+{expected_rows} rows")
        return
    if len(hypothesis_rows) != expected_rows or len(loop0_rows) != expected_rows:
        errors.append(
            "v58 archive counts are not an authorized cohort: "
            f"{len(hypothesis_rows)}+{len(loop0_rows)}, expected {expected_rows}+{expected_rows}"
        )
        return
    if hypothesis_ids != loop0_ids or len(set(hypothesis_ids)) != expected_rows:
        errors.append("v58 archive ID sets are not identical and unique")
    if digest != expected_digest:
        errors.append(f"v58 archive ID digest is not the authorized production identity (got {digest})")
    if full_tuple_digest != expected_full_tuple_digest:
        errors.append(
            "v58 archive full source-evidence digest is not authorized "
            f"(got {full_tuple_digest})"
        )

    by_segment = loop0_by_segment
    shaped = 0
    uuid_shape = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
    for hypothesis in hypothesis_rows:
        (
            rowid,
            segment_id,
            model_id,
            _transcript,
            confidence,
            created_at,
            model_version_id,
            source,
            reason,
            version,
            archived,
        ) = hypothesis
        loop0 = by_segment.get(str(segment_id))
        if loop0 is None:
            continue
        loop_id, _loop_segment, memory_fired, loop_created, loop_source, loop_reason, loop_version, loop_archived = loop0
        if (
            uuid_shape.fullmatch(str(segment_id))
            and int(rowid) - int(loop_id) == 2_555
            and model_id == "omniasr-7b-legacy-c348ade8a816"
            and model_version_id == "omniasr-7b-legacy-c348ade8a816"
            and confidence is None
            and created_at is not None
            and source == "segment_hypotheses"
            and reason == "missing speech_segments parent"
            and version == 58
            and archived is not None
            and memory_fired == 0
            and loop_created is not None
            and loop_source == "loop0_shadow_log"
            and loop_reason == "missing speech_segments parent"
            and loop_version == 58
            and loop_archived is not None
        ):
            shaped += 1
    if shaped != expected_rows:
        errors.append(f"v58 archive provenance/shape mismatch: {shaped}/{expected_rows} rows match")


def audit(
    db_path: Path,
    migrations_path: Path = DEFAULT_MIGRATIONS,
    *,
    require_production_v58_repair: bool = False,
    expected_v58_rows: int = V58_PRODUCTION_ARCHIVE_ROWS,
    expected_v58_digest: str = V58_PRODUCTION_ID_SHA256,
    expected_v58_full_tuple_digest: str = V58_PRODUCTION_FULL_TUPLE_SHA256,
) -> dict[str, object]:
    result: dict[str, object] = {
        "database": str(db_path),
        "ok": False,
        "errors": [],
    }
    errors: list[str] = result["errors"]  # type: ignore[assignment]
    if not db_path.is_file():
        errors.append("live database does not exist")
        return result

    uri = f"file:{db_path.resolve().as_posix()}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=30)
    except sqlite3.Error as error:
        errors.append(f"live database cannot be opened read-only: {error}")
        return result

    try:
        connection.execute("PRAGMA query_only=ON")
        # Pin every read below to one SQLite snapshot. Without an explicit read transaction, a writer
        # could commit between quick_check, foreign_key_check and migration-history reads and the gate
        # would certify a composite database state that never existed.
        connection.execute("BEGIN")
        quick = [str(row[0]) for row in connection.execute("PRAGMA quick_check")]
        full = [str(row[0]) for row in connection.execute("PRAGMA integrity_check")]
        violations = list(connection.execute("PRAGMA foreign_key_check"))
        by_table = Counter(str(row[0]) for row in violations)
        by_parent = Counter(str(row[2]) for row in violations)

        result.update(
            {
                "quickCheck": quick,
                "integrityCheck": full,
                "foreignKeyViolations": len(violations),
                "foreignKeyViolationsByTable": dict(sorted(by_table.items())),
                "foreignKeyViolationsByParent": dict(sorted(by_parent.items())),
            }
        )
        try:
            actual_migrations = [
                (int(version), str(description))
                for version, description in connection.execute(
                    "SELECT version, description FROM schema_migrations ORDER BY version"
                )
            ]
            required_migrations = source_migrations(migrations_path)
            schema_version = actual_migrations[-1][0] if actual_migrations else 0
            source_schema = required_migrations[-1][0]
            result["schemaVersion"] = schema_version
            result["requiredSchemaVersion"] = source_schema
            result["migrationHistoryEntries"] = len(actual_migrations)
            result["requiredMigrationHistoryEntries"] = len(required_migrations)
            if actual_migrations != required_migrations:
                actual_by_version = dict(actual_migrations)
                required_by_version = dict(required_migrations)
                missing = sorted(set(required_by_version) - set(actual_by_version))
                unknown = sorted(set(actual_by_version) - set(required_by_version))
                mismatched = sorted(
                    version
                    for version in set(actual_by_version) & set(required_by_version)
                    if actual_by_version[version] != required_by_version[version]
                )
                errors.append(
                    "live migration history does not exactly equal this release: "
                    f"schema {schema_version}/{source_schema}, missing={missing}, unknown={unknown}, "
                    f"descriptionMismatch={mismatched}"
                )
            if schema_version >= 58:
                _audit_v58_evidence(
                    connection,
                    result,
                    errors,
                    require_production_v58_repair,
                    expected_v58_rows,
                    expected_v58_digest,
                    expected_v58_full_tuple_digest,
                )
        except (OSError, ValueError) as error:
            errors.append(f"latest source migration cannot be resolved: {error}")
        except sqlite3.Error as error:
            errors.append(f"schema_migrations cannot be read: {error}")

        if quick != ["ok"]:
            errors.append(f"PRAGMA quick_check returned {quick!r}, expected ['ok']")
        if full != ["ok"]:
            errors.append(f"PRAGMA integrity_check returned {full!r}, expected ['ok']")
        if violations:
            detail = ", ".join(f"{table}={count}" for table, count in sorted(by_table.items()))
            errors.append(f"PRAGMA foreign_key_check found {len(violations)} violation(s): {detail}")
    except sqlite3.Error as error:
        errors.append(f"integrity audit failed: {error}")
    finally:
        try:
            connection.rollback()
        except sqlite3.Error:
            pass
        connection.close()

    result["ok"] = not errors
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=default_db_path())
    parser.add_argument("--migrations", type=Path, default=DEFAULT_MIGRATIONS)
    parser.add_argument(
        "--require-production-v58-repair",
        action="store_true",
        help="require the exact 2,104+2,104 production orphan-repair evidence cohort",
    )
    args = parser.parse_args(argv)
    report = audit(
        args.db,
        args.migrations,
        require_production_v58_repair=args.require_production_v58_repair,
    )
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
