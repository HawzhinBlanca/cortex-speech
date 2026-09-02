#!/usr/bin/env python3
"""Read-only SQLite inspection and exact schema/campaign contract checks."""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
from pathlib import Path
from typing import Any, Callable, Mapping
from urllib.parse import quote

from owner_proof_contract import expect_keys
from owner_proof_platform import ProofInputError


CAMPAIGN_SETTING_KEYS = (
    "review_campaign.sequential_first_pass.v1",
    "review_campaign.sequential_progress.v1",
)
CAMPAIGN_TABLES = (
    "review_campaign_registry",
    "review_campaign_focus",
    "review_campaign_transitions",
    "independent_review_decisions",
    "independent_review_reversals",
    "review_campaign_adjudications",
    "review_pool_registry",
    "review_pool_members",
    "review_pool_decisions",
    "review_pool_reversals",
    "review_pool_owner_adjudications",
    "review_pool_voice_certificates",
    "review_pool_dedup_manifests",
    "review_pool_duplicate_exclusions",
)


def sidecar(path: Path, suffix: str) -> Path:
    return Path(os.fspath(path) + suffix)


def reject_sqlite_sidecars(path: Path) -> None:
    present = [suffix for suffix in ("-wal", "-shm", "-journal") if os.path.lexists(sidecar(path, suffix))]
    if present:
        raise ProofInputError(f"SQLite authority has sidecars and is not a stable single-file clone: {present}")


def ascii_lower(value: str) -> str:
    return value.translate(str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz"))


def normalized_schema_sql(name: str, sql: str | None) -> str:
    if name.startswith("segments_fts_"):
        return "<sqlite-fts5-shadow>"
    return ascii_lower(" ".join((sql or "").strip().rstrip(";").split()))


def schema_fingerprint(connection: sqlite3.Connection) -> str:
    rows = [
        [str(kind), str(name), str(table), normalized_schema_sql(str(name), None if sql is None else str(sql))]
        for kind, name, table, sql in connection.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name"
        )
    ]
    encoded = json.dumps(rows, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def table_exists(connection: sqlite3.Connection, table: str) -> bool:
    row = connection.execute(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)",
        (table,),
    ).fetchone()
    return row is not None and int(row[0]) == 1


def inspect_sqlite_readonly(
    path: Path,
    *,
    assert_safe_existing_file: Callable[..., Path],
    hash_stable_file: Callable[[Path], tuple[str, int, int]],
    absolute_lexical: Callable[[Path], Path],
) -> dict[str, Any]:
    assert_safe_existing_file(path, role="database")
    reject_sqlite_sidecars(path)
    before_hash, _size, _mode = hash_stable_file(path)
    encoded = quote(absolute_lexical(path).as_posix(), safe="/:")
    try:
        connection = sqlite3.connect(f"file:{encoded}?mode=ro&immutable=1", uri=True, timeout=30, isolation_level=None)
    except sqlite3.Error as error:
        raise ProofInputError("database cannot be opened with immutable read authority") from error
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        quick = [str(row[0]) for row in connection.execute("PRAGMA quick_check")]
        full = [str(row[0]) for row in connection.execute("PRAGMA integrity_check")]
        foreign_keys = sum(1 for _row in connection.execute("PRAGMA foreign_key_check"))
        migrations = [
            (int(version), str(description))
            for version, description in connection.execute(
                "SELECT version, description FROM schema_migrations ORDER BY version"
            )
        ]
        if not migrations or [version for version, _description in migrations] != list(
            range(1, migrations[-1][0] + 1)
        ):
            raise ProofInputError("database migration history is not a contiguous prefix")
        segment_count = int(connection.execute("SELECT COUNT(*) FROM speech_segments").fetchone()[0])
        distinct_paths = int(
            connection.execute(
                "SELECT COUNT(DISTINCT audio_path) FROM speech_segments WHERE trim(audio_path) <> ''"
            ).fetchone()[0]
        )
        counts: dict[str, int] = {
            "settings": int(
                connection.execute(
                    "SELECT COUNT(*) FROM settings WHERE key IN (?, ?)",
                    CAMPAIGN_SETTING_KEYS,
                ).fetchone()[0]
            )
        }
        for table in CAMPAIGN_TABLES:
            counts[table] = (
                int(connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
                if table_exists(connection, table)
                else 0
            )
        inspection = {
            "schema": 1,
            "schemaVersion": migrations[-1][0],
            "migrationHistoryEntries": len(migrations),
            "schemaFingerprintSha256": schema_fingerprint(connection),
            "quickCheck": quick,
            "integrityCheck": full,
            "foreignKeyViolations": foreign_keys,
            "segmentCount": segment_count,
            "distinctAudioPathCount": distinct_paths,
            "sequentialCampaignPresent": counts["settings"] > 0,
            "reviewPoolPresent": counts["review_pool_registry"] > 0,
            "campaignAuthorityRows": sum(counts.values()),
            "campaignAuthorityCounts": dict(sorted(counts.items())),
        }
        if quick != ["ok"] or full != ["ok"] or foreign_keys != 0:
            raise ProofInputError("database integrity or foreign-key proof failed")
    except sqlite3.Error as error:
        raise ProofInputError("database aggregate inspection failed") from error
    finally:
        try:
            connection.rollback()
        except sqlite3.Error:
            pass
        connection.close()
    after_hash, _size, _mode = hash_stable_file(path)
    if after_hash != before_hash:
        raise ProofInputError("database changed during immutable inspection")
    reject_sqlite_sidecars(path)
    return inspection


def validate_inspection(
    inspection: Mapping[str, Any],
    *,
    expected_schema: int,
    expected_schema_fingerprint: str,
    expected_segments: int,
    expected_distinct_paths: int,
    campaign: str,
) -> None:
    required = {
        "schema",
        "schemaVersion",
        "migrationHistoryEntries",
        "schemaFingerprintSha256",
        "quickCheck",
        "integrityCheck",
        "foreignKeyViolations",
        "segmentCount",
        "distinctAudioPathCount",
        "sequentialCampaignPresent",
        "reviewPoolPresent",
        "campaignAuthorityRows",
        "campaignAuthorityCounts",
    }
    expect_keys(inspection, required, context="database inspection")
    if (
        inspection["schema"] != 1
        or inspection["schemaVersion"] != expected_schema
        or inspection["migrationHistoryEntries"] != expected_schema
        or inspection["segmentCount"] != expected_segments
        or inspection["distinctAudioPathCount"] != expected_distinct_paths
        or inspection["quickCheck"] != ["ok"]
        or inspection["integrityCheck"] != ["ok"]
        or inspection["foreignKeyViolations"] != 0
    ):
        raise ProofInputError("database schema, integrity, or aggregate counts do not match the exact contract")
    if inspection["schemaFingerprintSha256"] != expected_schema_fingerprint:
        raise ProofInputError("database schema fingerprint differs from the immutable contract")
    if campaign == "absent":
        if inspection["sequentialCampaignPresent"] or inspection["reviewPoolPresent"] or inspection[
            "campaignAuthorityRows"
        ] != 0:
            raise ProofInputError("scale database contains campaign authority")
    elif campaign == "required":
        if not inspection["sequentialCampaignPresent"] or inspection["campaignAuthorityRows"] <= 0:
            raise ProofInputError("campaign-exact database lacks sequential campaign authority")
    else:
        raise ProofInputError("unsupported campaign inspection mode")
