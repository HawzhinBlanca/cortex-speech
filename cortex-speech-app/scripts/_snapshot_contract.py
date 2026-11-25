#!/usr/bin/env python3
"""Shared snapshot schema/evidence constants for snapshot creation and restore drills."""

from __future__ import annotations

from review_pilot_hidden_contract import HIDDEN_KEY_SCHEMA_VERSION, HIDDEN_TABLE as HIDDEN_KEY_TABLE

DB_FILE = "cortex-speech.db"
# Each state has a mandatory representation: the real JSON file or its exact absence marker.
REQUIRED_STATE = (
    "settings.json",
    "champion.json",
    "reviewer_dialects.json",
    "voice_focus.json",
)
REVIEW_PILOT_FILE = "review_pilot_policy.json"
REVIEW_PILOT_ABSENT_FILE = "review_pilot_policy.absent"
REVIEW_PILOT_ABSENT_BYTES = b"review-pilot-policy-absent-v1\n"
MANIFEST_FILE = "SNAPSHOT_MANIFEST.json"
HUMAN_TABLES = ("speech_segments", "review_events", "spot_checks", "model_versions")
BASE_EVIDENCE_TABLES = HUMAN_TABLES + ("import_jobs", "import_job_files")
CAMPAIGN_SCHEMA_VERSION = 61
CAMPAIGN_EVIDENCE_TABLES = (
    "review_campaign_registry",
    "review_campaign_focus",
    "review_campaign_transitions",
    "independent_review_decisions",
    "independent_review_reversals",
    "review_campaign_adjudications",
)
POOL_SCHEMA_VERSION = 62
POOL_EVIDENCE_TABLES = (
    "review_pool_registry",
    "review_pool_members",
    "review_pool_decisions",
    "review_pool_reversals",
)
POOL_RESOLUTION_SCHEMA_VERSION = 63
POOL_RESOLUTION_EVIDENCE_TABLES = (
    "review_pool_owner_adjudications",
    "review_pool_voice_certificates",
)
POOL_DEDUP_SCHEMA_VERSION = 64
POOL_DEDUP_EVIDENCE_TABLES = (
    "review_pool_dedup_manifests",
    "review_pool_duplicate_exclusions",
)
POOL_DEDUP_SUPERSESSION_SCHEMA_VERSION = 70
POOL_DEDUP_SUPERSESSION_EVIDENCE_TABLES = ("review_pool_dedup_supersessions",)
# Keep this list identical to SnapshotRowCounts in src-tauri/src/snapshot.rs.
TRAINING_QUARANTINE_SCHEMA_VERSION = 72
TRAINING_QUARANTINE_EVIDENCE_TABLES = ("training_quarantine_holds", "training_quarantine_clearances")
EXPORT_BATCH_SCHEMA_VERSION = 73
EXPORT_BATCH_EVIDENCE_TABLES = ("review_pool_export_batches", "review_pool_export_batch_members")
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


def evidence_tables_for_schema(schema_version: int) -> tuple[str, ...]:
    """Preserve old evidence shapes and bind every durable review authority through v73."""
    tables = BASE_EVIDENCE_TABLES
    if schema_version >= HIDDEN_KEY_SCHEMA_VERSION:
        tables += (HIDDEN_KEY_TABLE,)
    if schema_version >= CAMPAIGN_SCHEMA_VERSION:
        tables += CAMPAIGN_EVIDENCE_TABLES
    if schema_version >= POOL_SCHEMA_VERSION:
        tables += POOL_EVIDENCE_TABLES
    if schema_version >= POOL_RESOLUTION_SCHEMA_VERSION:
        tables += POOL_RESOLUTION_EVIDENCE_TABLES
    if schema_version >= POOL_DEDUP_SCHEMA_VERSION:
        tables += POOL_DEDUP_EVIDENCE_TABLES
    if schema_version >= POOL_DEDUP_SUPERSESSION_SCHEMA_VERSION:
        tables += POOL_DEDUP_SUPERSESSION_EVIDENCE_TABLES
    if schema_version >= TRAINING_QUARANTINE_SCHEMA_VERSION:
        tables += TRAINING_QUARANTINE_EVIDENCE_TABLES
    if schema_version >= EXPORT_BATCH_SCHEMA_VERSION:
        tables += EXPORT_BATCH_EVIDENCE_TABLES
    return tables


def state_absence_marker(name: str) -> str:
    return f"{name}.absent"


def state_absence_bytes(name: str) -> bytes:
    return f"cortex-snapshot-state-absent-v1:{name}\n".encode("ascii")
