#!/usr/bin/env python3
"""Shared fail-closed contract for controlled-pilot hidden-check reservations.

The SQLite table is the lifetime authority.  ``couch_session.json`` is only a cache and may contain
at most a subset of the durable grants.  Keeping this contract in one module prevents the spot-check,
database-integrity, and compensation release gates from silently disagreeing about schema v59.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import uuid
from dataclasses import dataclass
from pathlib import Path

from pilot_focus_contract import PLAYBACK_GUARD_VERSION


# Operational paid-review code requires the complete v60 effect graph.  The durable hidden-key
# authority itself was introduced one migration earlier; recovery code must keep that historical
# boundary separate so it can still verify honest v59 snapshots instead of pretending the table
# first appeared in v60.
REQUIRED_SCHEMA = 60
HIDDEN_KEY_SCHEMA_VERSION = 59
COMPENSATION_POLICY_VERSION = "review-iqd-v1-2026-08-21"
POLICY_FILE = "review_pilot_policy.json"
SESSION_FILE = "couch_session.json"
POLICY_SCHEMA_VERSION = 1
PILOT_REVIEWERS = ("Rubar", "Alle")
CORPUS_ACTIONS_PER_REVIEWER = 10
TOTAL_CORPUS_ACTIONS = 20
HIDDEN_KEYS_PER_REVIEWER = 2
TOTAL_HIDDEN_KEYS = 4
MAX_UI_ACTIONS = TOTAL_CORPUS_ACTIONS + TOTAL_HIDDEN_KEYS

HIDDEN_TABLE = "review_pilot_hidden_keys"
HIDDEN_TABLE_SQL = """CREATE TABLE review_pilot_hidden_keys (
    policy_sha256 TEXT NOT NULL
        CHECK(length(policy_sha256) = 64 AND policy_sha256 NOT GLOB '*[^0-9a-f]*'),
    after_review_event_id INTEGER NOT NULL
        CHECK(after_review_event_id >= 0),
    reviewer TEXT NOT NULL COLLATE NOCASE
        CHECK(reviewer = trim(reviewer) AND length(reviewer) BETWEEN 1 AND 40),
    segment_id TEXT NOT NULL
        CHECK(segment_id = trim(segment_id) AND length(segment_id) BETWEEN 1 AND 256),
    PRIMARY KEY(policy_sha256, after_review_event_id, reviewer, segment_id)
) STRICT"""
HIDDEN_TRIGGER_SQL = {
    "review_pilot_hidden_keys_policy_insert": """CREATE TRIGGER review_pilot_hidden_keys_policy_insert
        BEFORE INSERT ON review_pilot_hidden_keys
        WHEN EXISTS (
            SELECT 1 FROM review_pilot_hidden_keys
             WHERE after_review_event_id = NEW.after_review_event_id
               AND policy_sha256 <> NEW.policy_sha256
        )
        BEGIN SELECT RAISE(ABORT, 'controlled review pilot baseline is bound to another policy'); END""",
    "review_pilot_hidden_keys_quota_insert": """CREATE TRIGGER review_pilot_hidden_keys_quota_insert
        BEFORE INSERT ON review_pilot_hidden_keys
        WHEN NOT EXISTS (
            SELECT 1 FROM review_pilot_hidden_keys
             WHERE policy_sha256 = NEW.policy_sha256
               AND after_review_event_id = NEW.after_review_event_id
               AND reviewer = NEW.reviewer
               AND segment_id = NEW.segment_id
        )
        AND (
            (SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = NEW.policy_sha256
                AND after_review_event_id = NEW.after_review_event_id
                AND reviewer = NEW.reviewer) >= 2
            OR
            (SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = NEW.policy_sha256
                AND after_review_event_id = NEW.after_review_event_id) >= 4
        )
        BEGIN SELECT RAISE(ABORT, 'controlled review pilot hidden-key quota exceeded'); END""",
    "review_pilot_hidden_keys_immutable_update": """CREATE TRIGGER review_pilot_hidden_keys_immutable_update
        BEFORE UPDATE ON review_pilot_hidden_keys
        BEGIN SELECT RAISE(ABORT, 'controlled review pilot hidden keys are append-only'); END""",
    "review_pilot_hidden_keys_immutable_delete": """CREATE TRIGGER review_pilot_hidden_keys_immutable_delete
        BEFORE DELETE ON review_pilot_hidden_keys
        BEGIN SELECT RAISE(ABORT, 'controlled review pilot hidden keys are append-only'); END""",
}
HIDDEN_SCHEMA_SQL = HIDDEN_TABLE_SQL + ";\n" + ";\n".join(HIDDEN_TRIGGER_SQL.values()) + ";"


class PilotContractError(RuntimeError):
    """The active paid-pilot state cannot be proved from durable evidence."""


@dataclass(frozen=True)
class ReviewPilotPolicy:
    after_review_event_id: int
    max_total_corpus_actions: int
    reviewer_caps: dict[str, int]


@dataclass(frozen=True)
class EffectiveReviewEvent:
    """One pilot-count event: every raw skip, or an effective non-skip decision."""

    event_id: int
    segment_id: str
    reviewer: str
    action: str
    source: str
    created_at: str
    timestamp_ms: object
    duration_ms: object
    compensation_action: str
    operation_id: str
    operation_payload_hash: str
    app_git_sha: str
    playback_guard_version: str
    ledger_id: int
    ledger_entry_id: str
    canonical_work_id: str
    canonical_identity_kind: str
    decision_revision: object


@dataclass(frozen=True)
class PilotReviewHistory:
    """Audited raw append-only history and its sole authoritative safety projection."""

    effective_events: tuple[EffectiveReviewEvent, ...]
    raw_original_count: int
    reversal_count: int
    effect_event_count: int
    effect_reversal_count: int


@dataclass(frozen=True)
class HiddenPilotState:
    policy_sha256: str
    grants: dict[str, set[str]]
    session_keys: dict[str, set[str]]
    completed_keys: dict[str, set[str]]
    skipped_keys: dict[str, set[str]]
    unresolved_keys: dict[str, set[str]]
    corpus_actions: dict[str, int]
    hidden_actions: dict[str, int]
    effective_events: tuple[EffectiveReviewEvent, ...] = ()
    raw_original_count: int = 0
    reversal_count: int = 0

    @property
    def total_corpus_actions(self) -> int:
        return sum(self.corpus_actions.values())

    @property
    def total_hidden_actions(self) -> int:
        return sum(self.hidden_actions.values())

    @property
    def total_ui_actions(self) -> int:
        return self.total_corpus_actions + self.total_hidden_actions


def strict_json_loads(raw: str) -> object:
    """Match serde_json's duplicate-field and non-finite-number rejection."""

    def object_without_duplicates(pairs: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON field {key!r}")
            value[key] = item
        return value

    def invalid_constant(value: str) -> object:
        raise ValueError(f"invalid JSON constant {value}")

    return json.loads(raw, object_pairs_hook=object_without_duplicates, parse_constant=invalid_constant)


def parse_policy(value: object, source: str = POLICY_FILE) -> ReviewPilotPolicy:
    if not isinstance(value, dict) or set(value) != {
        "schema_version",
        "after_review_event_id",
        "max_total_corpus_actions",
        "reviewers",
    }:
        raise PilotContractError(f"{source} has missing or unknown fields")
    if type(value["schema_version"]) is not int or value["schema_version"] != POLICY_SCHEMA_VERSION:
        raise PilotContractError(f"{source} schema_version must be {POLICY_SCHEMA_VERSION}")
    after = value["after_review_event_id"]
    total = value["max_total_corpus_actions"]
    if type(after) is not int or after < 0:
        raise PilotContractError(f"{source} after_review_event_id must be a non-negative integer")
    if type(total) is not int or total != TOTAL_CORPUS_ACTIONS:
        raise PilotContractError(f"{source} must cap exactly {TOTAL_CORPUS_ACTIONS} corpus actions")
    entries = value["reviewers"]
    if not isinstance(entries, list) or len(entries) != len(PILOT_REVIEWERS):
        raise PilotContractError(f"{source} must name exactly {len(PILOT_REVIEWERS)} reviewers")
    actual: dict[str, int] = {}
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"name", "max_corpus_actions"}:
            raise PilotContractError(f"{source} has an invalid reviewer entry")
        name = entry["name"]
        cap = entry["max_corpus_actions"]
        if (
            not isinstance(name, str)
            or not name.strip()
            or len(name.strip()) > 40
            or any(ord(char) < 32 or ord(char) == 127 for char in name)
            or type(cap) is not int
        ):
            raise PilotContractError(f"{source} has invalid reviewer values")
        key = name.strip().lower()
        if key in actual:
            raise PilotContractError(f"{source} reviewer names must be distinct")
        actual[key] = cap
    expected = {name.lower(): CORPUS_ACTIONS_PER_REVIEWER for name in PILOT_REVIEWERS}
    if actual != expected:
        raise PilotContractError(
            f"{source} must name exactly {' and '.join(PILOT_REVIEWERS)} at "
            f"{CORPUS_ACTIONS_PER_REVIEWER} actions each"
        )
    return ReviewPilotPolicy(
        after_review_event_id=after,
        max_total_corpus_actions=total,
        reviewer_caps={name: CORPUS_ACTIONS_PER_REVIEWER for name in PILOT_REVIEWERS},
    )


def read_policy(path: Path) -> ReviewPilotPolicy:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as error:
        raise PilotContractError(f"{path.name} is unreadable: {error}") from error
    try:
        return parse_policy(strict_json_loads(raw), path.name)
    except (json.JSONDecodeError, ValueError) as error:
        raise PilotContractError(f"{path.name} is invalid JSON: {error}") from error


def policy_sha256(policy: ReviewPilotPolicy) -> str:
    """Mirror ``ReviewPilotPolicy::policy_sha256`` byte for byte."""
    digest = hashlib.sha256()
    digest.update(b"cortex-review-pilot-policy-v1\0")
    digest.update(POLICY_SCHEMA_VERSION.to_bytes(4, "big", signed=False))
    digest.update(policy.after_review_event_id.to_bytes(8, "big", signed=True))
    digest.update(policy.max_total_corpus_actions.to_bytes(8, "big", signed=True))
    entries = sorted(policy.reviewer_caps.items(), key=lambda item: item[0].lower())
    digest.update(len(entries).to_bytes(8, "big", signed=False))
    for name, cap in entries:
        # The authorized roster is ASCII.  Rust deliberately uses to_ascii_lowercase, not Unicode
        # case folding, so encode the same semantic identity here.
        canonical = "".join(char.lower() if "A" <= char <= "Z" else char for char in name)
        name_bytes = canonical.encode("utf-8")
        digest.update(len(name_bytes).to_bytes(8, "big", signed=False))
        digest.update(name_bytes)
        digest.update(cap.to_bytes(8, "big", signed=True))
    return digest.hexdigest()


def normalized_sql(value: str) -> str:
    return " ".join(value.strip().rstrip(";").lower().split())


def audit_hidden_schema(connection: sqlite3.Connection) -> tuple[dict[str, object], list[str]]:
    """Prove the exact v59 table/triggers and recheck quotas from stored rows."""
    evidence: dict[str, object] = {}
    errors: list[str] = []
    try:
        schema_version = int(
            connection.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
        )
    except (sqlite3.Error, TypeError, ValueError) as error:
        return evidence, [f"schema version cannot be read for hidden-key reservations: {error}"]
    evidence["pilotHiddenSchemaVersion"] = schema_version
    if schema_version < REQUIRED_SCHEMA:
        errors.append(f"schema {schema_version} is older than durable hidden-key migration {REQUIRED_SCHEMA}")
        return evidence, errors

    try:
        table = connection.execute(
            "SELECT type, tbl_name, sql FROM sqlite_schema WHERE name = ?",
            (HIDDEN_TABLE,),
        ).fetchone()
        if table is None:
            errors.append(f"schema {schema_version} is missing {HIDDEN_TABLE}")
            return evidence, errors
        if (
            str(table[0]) != "table"
            or str(table[1]) != HIDDEN_TABLE
            or normalized_sql(str(table[2] or "")) != normalized_sql(HIDDEN_TABLE_SQL)
        ):
            errors.append(f"{HIDDEN_TABLE} does not exactly match the schema v59 contract")

        actual_triggers = {
            str(name): str(sql or "")
            for name, sql in connection.execute(
                "SELECT name, sql FROM sqlite_schema WHERE type = 'trigger' AND tbl_name = ?",
                (HIDDEN_TABLE,),
            )
        }
        expected_names = set(HIDDEN_TRIGGER_SQL)
        missing = sorted(expected_names - set(actual_triggers))
        unexpected = sorted(set(actual_triggers) - expected_names)
        if missing:
            errors.append(f"v59 hidden-key trigger(s) missing: {missing}")
        if unexpected:
            errors.append(f"v59 hidden-key trigger(s) unexpected: {unexpected}")
        for name in sorted(expected_names & set(actual_triggers)):
            if normalized_sql(actual_triggers[name]) != normalized_sql(HIDDEN_TRIGGER_SQL[name]):
                errors.append(f"v59 hidden-key trigger {name} does not exactly match its contract")
        evidence["pilotHiddenTriggers"] = sorted(actual_triggers)

        malformed = int(
            connection.execute(
                """SELECT COUNT(*) FROM review_pilot_hidden_keys
                    WHERE length(policy_sha256) <> 64
                       OR policy_sha256 GLOB '*[^0-9a-f]*'
                       OR after_review_event_id < 0
                       OR reviewer <> trim(reviewer) OR length(reviewer) NOT BETWEEN 1 AND 40
                       OR segment_id <> trim(segment_id) OR length(segment_id) NOT BETWEEN 1 AND 256"""
            ).fetchone()[0]
        )
        reviewer_overages = list(
            connection.execute(
                """SELECT policy_sha256, after_review_event_id, reviewer, COUNT(*)
                     FROM review_pilot_hidden_keys
                    GROUP BY policy_sha256, after_review_event_id, reviewer
                   HAVING COUNT(*) > ?""",
                (HIDDEN_KEYS_PER_REVIEWER,),
            )
        )
        namespace_overages = list(
            connection.execute(
                """SELECT policy_sha256, after_review_event_id, COUNT(*)
                     FROM review_pilot_hidden_keys
                    GROUP BY policy_sha256, after_review_event_id
                   HAVING COUNT(*) > ?""",
                (TOTAL_HIDDEN_KEYS,),
            )
        )
        row_count = int(connection.execute("SELECT COUNT(*) FROM review_pilot_hidden_keys").fetchone()[0])
        namespace_count = int(
            connection.execute(
                "SELECT COUNT(*) FROM (SELECT 1 FROM review_pilot_hidden_keys GROUP BY policy_sha256, after_review_event_id)"
            ).fetchone()[0]
        )
        evidence.update(
            {
                "pilotHiddenRows": row_count,
                "pilotHiddenNamespaces": namespace_count,
                "pilotHiddenMalformedRows": malformed,
                "pilotHiddenReviewerOverages": len(reviewer_overages),
                "pilotHiddenNamespaceOverages": len(namespace_overages),
            }
        )
        if malformed:
            errors.append(f"v59 hidden-key table contains {malformed} malformed row(s)")
        if reviewer_overages:
            errors.append(f"v59 hidden-key table exceeds max 2 for {len(reviewer_overages)} reviewer namespace(s)")
        if namespace_overages:
            errors.append(f"v59 hidden-key table exceeds max 4 for {len(namespace_overages)} policy namespace(s)")
    except sqlite3.Error as error:
        errors.append(f"v59 hidden-key schema/evidence cannot be read: {error}")
    return evidence, errors


def _canonical_reviewer(policy: ReviewPilotPolicy, actual: object, source: str) -> str:
    if not isinstance(actual, str) or not actual.strip():
        raise PilotContractError(f"{source} has an invalid reviewer")
    matches = [name for name in policy.reviewer_caps if name.lower() == actual.strip().lower()]
    if len(matches) != 1:
        raise PilotContractError(f"{source} contains unauthorized reviewer {actual!r}")
    return matches[0]


def _same_path(left: str, right: Path) -> bool:
    return os.path.normcase(os.path.abspath(left)) == os.path.normcase(os.path.abspath(right))


def _dict_rows(cursor: sqlite3.Cursor) -> list[dict[str, object]]:
    columns = [str(item[0]) for item in cursor.description or ()]
    return [dict(zip(columns, tuple(row), strict=True)) for row in cursor.fetchall()]


def _is_lower_hex(value: object, length: int) -> bool:
    return (
        isinstance(value, str)
        and len(value) == length
        and all(character in "0123456789abcdef" for character in value)
    )


def _is_canonical_uuid(value: object) -> bool:
    if not isinstance(value, str):
        return False
    try:
        return str(uuid.UUID(value)) == value
    except ValueError:
        return False


def audit_pilot_review_history(
    connection: sqlite3.Connection,
    policy: ReviewPilotPolicy,
) -> PilotReviewHistory:
    """Audit every v60 paid original/reversal, then return only the effective projection.

    Raw rows remain evidence, never authority.  A raw corpus judgement may disappear from the pilot
    count only through one exact compensation reversal plus its exact effect reversal.  Corpus skips
    always consume their safety slot; hidden spot-check rows are effectless and immutable.  This
    prevents an unpaired later row from silently shadowing work through the SQL view.
    """
    try:
        schema_version = int(
            connection.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
        )
        if schema_version != REQUIRED_SCHEMA:
            raise PilotContractError(
                f"controlled review requires exact schema {REQUIRED_SCHEMA}, found {schema_version}"
            )
        state_rows = list(
            connection.execute(
                """SELECT effective_after_review_event_id, effective_after_ledger_id
                     FROM review_effect_state WHERE singleton_key = 1"""
            )
        )
        if len(state_rows) != 1:
            raise PilotContractError("schema v60 review-effect cutoff singleton is missing or ambiguous")
        effect_cutoff = int(state_rows[0][0])
        ledger_cutoff = int(state_rows[0][1])
        if effect_cutoff != policy.after_review_event_id:
            raise PilotContractError(
                "active pilot baseline does not equal the immutable schema-v60 review-effect cutoff"
            )

        raw_events = _dict_rows(
            connection.execute(
                """SELECT id, segment_id, reviewer, action, compensation_action, source,
                          timestamp_ms, created_at, duration_ms, operation_id,
                          operation_payload_hash, app_git_sha, playback_guard_version
                     FROM review_events
                    WHERE id > ? AND source IN ('couch', 'couch_spot_check')
                    ORDER BY id""",
                (effect_cutoff,),
            )
        )
        event_by_id = {int(row["id"]): row for row in raw_events}
        for event_id, event in event_by_id.items():
            if not _is_lower_hex(event["app_git_sha"], 40):
                raise PilotContractError(
                    f"post-v60 paid event {event_id} lacks an exact 40-lowerhex app_git_sha"
                )
            if event["playback_guard_version"] != PLAYBACK_GUARD_VERSION:
                raise PilotContractError(
                    f"post-v60 paid event {event_id} playback guard is not {PLAYBACK_GUARD_VERSION!r}"
                )
            if not _is_canonical_uuid(event["operation_id"]):
                raise PilotContractError(f"post-v60 paid event {event_id} lacks a canonical operation UUID")
            if not _is_lower_hex(event["operation_payload_hash"], 64):
                raise PilotContractError(f"post-v60 paid event {event_id} lacks a canonical payload hash")

        ledger_rows = _dict_rows(
            connection.execute(
                """SELECT id, entry_id, entry_key, policy_version, review_event_id,
                          canonical_work_id, canonical_identity_kind, reviewer, segment_id,
                          source, compensation_action, effective_decision, decision_revision,
                          duration_ms, rate_basis_points, entitlement_micro_iqd,
                          delta_micro_iqd, corrected_entitlement_ms, delta_corrected_ms,
                          created_at, reverses_entry_id
                     FROM review_compensation_ledger
                    WHERE id > ?
                       OR review_event_id IN (
                            SELECT id FROM review_events
                             WHERE id > ? AND source IN ('couch', 'couch_spot_check')
                       )
                    ORDER BY id""",
                (ledger_cutoff, effect_cutoff),
            )
        )
        originals_by_event: dict[int, list[dict[str, object]]] = {}
        original_by_entry: dict[str, dict[str, object]] = {}
        reversals: list[dict[str, object]] = []
        for row in ledger_rows:
            event_id = row["review_event_id"]
            reverses = row["reverses_entry_id"]
            if event_id is not None and reverses is None:
                originals_by_event.setdefault(int(event_id), []).append(row)
                original_by_entry[str(row["entry_id"])] = row
            elif event_id is None and reverses is not None:
                reversals.append(row)
            else:
                raise PilotContractError(
                    f"post-v60 ledger row {row['entry_id']} is neither one original nor one reversal"
                )

        originals: dict[int, dict[str, object]] = {}
        for event_id, event in event_by_id.items():
            rows = originals_by_event.get(event_id, [])
            if len(rows) != 1:
                raise PilotContractError(
                    f"post-v60 paid event {event_id} has {len(rows)} original ledger rows; required exactly one"
                )
            row = rows[0]
            originals[event_id] = row
            if row["policy_version"] != COMPENSATION_POLICY_VERSION:
                raise PilotContractError(
                    f"post-v60 paid event {event_id} is bound to inactive policy {row['policy_version']!r}"
                )
            expected = (
                row["entry_key"] == f"review-event:{event_id}"
                and row["segment_id"] == event["segment_id"]
                and isinstance(row["reviewer"], str)
                and isinstance(event["reviewer"], str)
                and str(row["reviewer"]).casefold() == str(event["reviewer"]).casefold()
                and row["source"] == event["source"]
                and row["compensation_action"] == event["compensation_action"]
                and row["effective_decision"] == event["action"]
                and row["duration_ms"] == event["duration_ms"]
            )
            if not expected:
                raise PilotContractError(
                    f"post-v60 paid event {event_id} and its original ledger identity disagree"
                )
            if type(row["decision_revision"]) is not int or int(row["decision_revision"]) < 0:
                raise PilotContractError(
                    f"post-v60 paid event {event_id} has an invalid ledger decision revision"
                )
        extra_originals = sorted(set(originals_by_event) - set(event_by_id))
        if extra_originals:
            raise PilotContractError(
                f"post-v60 ledger original points outside paid pilot history: event {extra_originals[0]}"
            )

        reversal_by_target: dict[str, dict[str, object]] = {}
        for reversal in reversals:
            entry_id = str(reversal["entry_id"])
            target_id = str(reversal["reverses_entry_id"])
            target = original_by_entry.get(target_id)
            if target is None:
                raise PilotContractError(
                    f"post-v60 reversal {entry_id} targets a missing/non-pilot original {target_id!r}"
                )
            target_event_id = int(target["review_event_id"])
            if event_by_id[target_event_id]["source"] == "couch_spot_check":
                raise PilotContractError(
                    f"hidden spot-check event {target_event_id} is immutable and cannot be reversed"
                )
            if target_id in reversal_by_target:
                raise PilotContractError(f"original ledger entry {target_id} has more than one reversal")
            operation_id = str(reversal["entry_key"] or "")
            operation_id = operation_id.removeprefix("undo:") if operation_id.startswith("undo:") else ""
            exact_pair = (
                _is_canonical_uuid(operation_id)
                and reversal["policy_version"] == target["policy_version"] == COMPENSATION_POLICY_VERSION
                and reversal["canonical_work_id"] == target["canonical_work_id"]
                and reversal["canonical_identity_kind"] == target["canonical_identity_kind"]
                and isinstance(reversal["reviewer"], str)
                and isinstance(target["reviewer"], str)
                and str(reversal["reviewer"]).casefold() == str(target["reviewer"]).casefold()
                and reversal["segment_id"] == target["segment_id"]
                and reversal["source"] == "couch_undo"
                and reversal["compensation_action"] == "undo"
                and reversal["effective_decision"] == "undo"
                and reversal["decision_revision"] == target["decision_revision"]
                and reversal["duration_ms"] == target["duration_ms"]
                and reversal["rate_basis_points"] == 0
                and reversal["entitlement_micro_iqd"] == 0
                and reversal["delta_micro_iqd"] == -int(target["delta_micro_iqd"])
                and reversal["delta_corrected_ms"] == -int(target["delta_corrected_ms"])
            )
            if not exact_pair:
                raise PilotContractError(
                    f"post-v60 reversal {entry_id} is not the exact inverse of original {target_id}"
                )
            reversal_by_target[target_id] = reversal

        effect_rows = _dict_rows(
            connection.execute(
                """SELECT id, review_event_id, segment_id, reviewer, source, action,
                          decision_revision, created_at
                     FROM human_decision_effect_events
                    WHERE review_event_id IS NOT NULL
                      AND review_event_id > ?
                    ORDER BY id""",
                (effect_cutoff,),
            )
        )
        effects_by_event: dict[int, list[dict[str, object]]] = {}
        effect_by_id: dict[int, dict[str, object]] = {}
        for effect in effect_rows:
            effect_id = int(effect["id"])
            effect_by_id[effect_id] = effect
            effects_by_event.setdefault(int(effect["review_event_id"]), []).append(effect)
        effect_reversals = _dict_rows(
            connection.execute(
                """SELECT r.effect_event_id, r.operation_id, r.created_at
                     FROM human_decision_effect_reversals r
                     JOIN human_decision_effect_events e ON e.id = r.effect_event_id
                    WHERE e.review_event_id IS NOT NULL AND e.review_event_id > ?
                    ORDER BY r.effect_event_id""",
                (effect_cutoff,),
            )
        )
        effect_reversal_by_id = {int(row["effect_event_id"]): row for row in effect_reversals}
        for event_id, event in event_by_id.items():
            effect_candidates = effects_by_event.get(event_id, [])
            action = str(event["action"])
            source = str(event["source"])
            requires_decision_effect = source == "couch" and action != "skip"
            if not requires_decision_effect:
                if effect_candidates:
                    kind = "hidden spot-check" if source == "couch_spot_check" else "skip"
                    raise PilotContractError(
                        f"{kind} event {event_id} unexpectedly has a human-decision effect"
                    )
                continue
            if len(effect_candidates) != 1:
                raise PilotContractError(
                    f"post-v60 judgement event {event_id} has {len(effect_candidates)} effect rows; required one"
                )
            effect = effect_candidates[0]
            effect_id = int(effect["id"])
            original = originals[event_id]
            if not (
                effect["segment_id"] == event["segment_id"]
                and isinstance(effect["reviewer"], str)
                and str(effect["reviewer"]).casefold() == str(event["reviewer"]).casefold()
                and effect["source"] == event["source"]
                and effect["action"] == action
                and effect["decision_revision"] == original["decision_revision"]
            ):
                raise PilotContractError(f"effect row {effect_id} disagrees with paid event {event_id}")
            reversal = reversal_by_target.get(str(original["entry_id"]))
            effect_reversal = effect_reversal_by_id.get(effect_id)
            if reversal is None and effect_reversal is not None:
                raise PilotContractError(f"active paid event {event_id} has a forged effect reversal")
            if reversal is not None:
                operation_id = str(reversal["entry_key"])[len("undo:") :]
                if effect_reversal is None or effect_reversal["operation_id"] != operation_id:
                    raise PilotContractError(
                        f"reversed paid event {event_id} lacks its exact operation-bound effect reversal"
                    )
        extra_effect_events = sorted(set(effects_by_event) - set(event_by_id))
        if extra_effect_events:
            raise PilotContractError(
                f"phone effect points outside post-v60 paid history: event {extra_effect_events[0]}"
            )

        effective_rows = _dict_rows(
            connection.execute(
                """SELECT review_event_id, segment_id, reviewer, action, source, timestamp_ms,
                          review_event_created_at, review_event_duration_ms,
                          review_event_compensation_action, operation_id, operation_payload_hash,
                          app_git_sha, playback_guard_version, ledger_id, ledger_entry_id,
                          canonical_work_id, canonical_identity_kind, decision_revision
                     FROM effective_review_events_v60
                    WHERE review_event_id > ? AND policy_version = ?
                      AND source IN ('couch', 'couch_spot_check')
                    ORDER BY review_event_id""",
                (effect_cutoff, COMPENSATION_POLICY_VERSION),
            )
        )
        effective_ids = {int(row["review_event_id"]) for row in effective_rows}
        effective_non_skip_ids = {
            int(row["review_event_id"]) for row in effective_rows if row["action"] != "skip"
        }
        expected_effective_non_skip_ids: set[int] = set()
        reversed_non_skip_count = 0
        for event_id, original in originals.items():
            reversed_row = reversal_by_target.get(str(original["entry_id"]))
            action = str(event_by_id[event_id]["action"])
            if action != "skip" and reversed_row is None:
                expected_effective_non_skip_ids.add(event_id)
            elif action != "skip" and reversed_row is not None:
                reversed_non_skip_count += 1
            if reversed_row is not None and event_id in effective_ids:
                raise PilotContractError(f"reversed paid event {event_id} remains in the effective view")
        if effective_non_skip_ids != expected_effective_non_skip_ids:
            missing = sorted(expected_effective_non_skip_ids - effective_non_skip_ids)
            unexpected = sorted(effective_non_skip_ids - expected_effective_non_skip_ids)
            raise PilotContractError(
                "effective non-skip view disagrees with exact reversals: "
                f"missing={missing}, unexpected={unexpected}"
            )
        active_work_ids: set[str] = set()
        for row in effective_rows:
            work_id = str(row["canonical_work_id"])
            if work_id in active_work_ids:
                raise PilotContractError(f"effective paid history repeats canonical work {work_id!r}")
            active_work_ids.add(work_id)
        raw_skip_count = sum(1 for event in raw_events if event["action"] == "skip")
        counted_event_count = raw_skip_count + len(effective_non_skip_ids)
        if len(raw_events) != counted_event_count + reversed_non_skip_count:
            raise PilotContractError(
                "raw non-skip history exceeds effective history without one exact reversal pair per extra original"
            )

        counted_rows = [row for row in effective_rows if row["action"] != "skip"]
        for event_id, event in event_by_id.items():
            if event["action"] != "skip":
                continue
            original = originals[event_id]
            counted_rows.append(
                {
                    "review_event_id": event_id,
                    "segment_id": event["segment_id"],
                    "reviewer": event["reviewer"],
                    "action": event["action"],
                    "source": event["source"],
                    "timestamp_ms": event["timestamp_ms"],
                    "review_event_created_at": event["created_at"],
                    "review_event_duration_ms": event["duration_ms"],
                    "review_event_compensation_action": event["compensation_action"],
                    "operation_id": event["operation_id"],
                    "operation_payload_hash": event["operation_payload_hash"],
                    "app_git_sha": event["app_git_sha"],
                    "playback_guard_version": event["playback_guard_version"],
                    "ledger_id": original["id"],
                    "ledger_entry_id": original["entry_id"],
                    "canonical_work_id": original["canonical_work_id"],
                    "canonical_identity_kind": original["canonical_identity_kind"],
                    "decision_revision": original["decision_revision"],
                }
            )
        counted_rows.sort(key=lambda row: int(row["review_event_id"]))
        effective_events = tuple(
            EffectiveReviewEvent(
                event_id=int(row["review_event_id"]),
                segment_id=str(row["segment_id"]),
                reviewer=str(row["reviewer"]),
                action=str(row["action"]),
                source=str(row["source"]),
                created_at=str(row["review_event_created_at"] or ""),
                timestamp_ms=row["timestamp_ms"],
                duration_ms=row["review_event_duration_ms"],
                compensation_action=str(row["review_event_compensation_action"] or ""),
                operation_id=str(row["operation_id"] or ""),
                operation_payload_hash=str(row["operation_payload_hash"] or ""),
                app_git_sha=str(row["app_git_sha"] or ""),
                playback_guard_version=str(row["playback_guard_version"] or ""),
                ledger_id=int(row["ledger_id"]),
                ledger_entry_id=str(row["ledger_entry_id"]),
                canonical_work_id=str(row["canonical_work_id"]),
                canonical_identity_kind=str(row["canonical_identity_kind"]),
                decision_revision=row["decision_revision"],
            )
            for row in counted_rows
        )
        return PilotReviewHistory(
            effective_events=effective_events,
            raw_original_count=len(raw_events),
            reversal_count=len(reversals),
            effect_event_count=len(effect_rows),
            effect_reversal_count=len(effect_reversals),
        )
    except PilotContractError:
        raise
    except (sqlite3.Error, TypeError, ValueError) as error:
        raise PilotContractError(f"schema-v60 effective review history cannot be proved: {error}") from error


def audit_active_hidden_state(
    connection: sqlite3.Connection,
    data_dir: Path,
    db_path: Path,
    policy: ReviewPilotPolicy,
) -> HiddenPilotState:
    """Bind policy, session cache, grants, results, skips, and action caps in one DB snapshot."""
    active_policy = read_policy(data_dir / POLICY_FILE)
    if active_policy != policy or policy_sha256(active_policy) != policy_sha256(policy):
        raise PilotContractError("active policy changed before hidden-key evidence was read")
    schema_evidence, schema_errors = audit_hidden_schema(connection)
    if schema_errors:
        raise PilotContractError("; ".join(schema_errors))
    del schema_evidence
    history = audit_pilot_review_history(connection, policy)

    digest = policy_sha256(policy)
    baseline = policy.after_review_event_id
    try:
        current_max = int(connection.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0])
        if baseline > current_max:
            raise PilotContractError("controlled-review baseline is ahead of durable review history")
        inconsistent_namespace = int(
            connection.execute(
                """SELECT COUNT(*) FROM review_pilot_hidden_keys
                    WHERE (policy_sha256 = ? OR after_review_event_id = ?)
                      AND NOT (policy_sha256 = ? AND after_review_event_id = ?)""",
                (digest, baseline, digest, baseline),
            ).fetchone()[0]
        )
        if inconsistent_namespace:
            raise PilotContractError(
                f"{inconsistent_namespace} hidden-key grant(s) disagree with the active policy SHA/baseline"
            )
        grants = {name: set() for name in policy.reviewer_caps}
        for actual, segment_id in connection.execute(
            """SELECT reviewer, segment_id FROM review_pilot_hidden_keys
                WHERE policy_sha256 = ? AND after_review_event_id = ?
                ORDER BY reviewer COLLATE NOCASE, segment_id""",
            (digest, baseline),
        ):
            reviewer = _canonical_reviewer(policy, actual, "durable hidden-key grants")
            segment = str(segment_id)
            if segment in grants[reviewer]:
                raise PilotContractError(f"duplicate durable hidden-key grant for {reviewer}/{segment}")
            grants[reviewer].add(segment)
        if any(len(ids) > HIDDEN_KEYS_PER_REVIEWER for ids in grants.values()):
            raise PilotContractError("durable hidden-key grants exceed the max-2 reviewer quota")
        if sum(map(len, grants.values())) > TOTAL_HIDDEN_KEYS:
            raise PilotContractError("durable hidden-key grants exceed the max-4 policy quota")
    except sqlite3.Error as error:
        raise PilotContractError(f"durable hidden-key grants cannot be read: {error}") from error

    session_path = data_dir / SESSION_FILE
    try:
        session_raw = session_path.read_text(encoding="utf-8")
        session = strict_json_loads(session_raw)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        raise PilotContractError(f"{SESSION_FILE} is unreadable or invalid: {error}") from error
    if not isinstance(session, dict):
        raise PilotContractError(f"{SESSION_FILE} root must be an object")
    recorded_db = session.get("db_path")
    if not isinstance(recorded_db, str) or not _same_path(recorded_db, db_path):
        raise PilotContractError(f"{SESSION_FILE} belongs to a different database")
    try:
        session_policy = parse_policy(session.get("pilot_policy"), f"{SESSION_FILE}.pilot_policy")
    except PilotContractError as error:
        raise PilotContractError(f"remembered session is not bound to the active policy: {error}") from error
    if session_policy != policy or policy_sha256(session_policy) != digest:
        raise PilotContractError("remembered session is not bound to the active controlled-pilot policy")

    pairing = session.get("reviewers")
    if not isinstance(pairing, dict) or not pairing or not all(
        isinstance(token, str) and token and isinstance(name, str) for token, name in pairing.items()
    ):
        raise PilotContractError(f"{SESSION_FILE} has invalid reviewer pairing state")
    paired = [_canonical_reviewer(policy, name, f"{SESSION_FILE}.reviewers") for name in pairing.values()]
    if sorted(name.lower() for name in paired) != sorted(name.lower() for name in policy.reviewer_caps):
        raise PilotContractError(f"{SESSION_FILE} reviewer roster does not exactly match the active pilot")

    entries = session.get("pilot_spot_checks")
    if not isinstance(entries, list):
        raise PilotContractError("remembered session has no valid pilot hidden-check cache")
    session_keys = {name: set() for name in policy.reviewer_caps}
    for entry in entries:
        if (
            not isinstance(entry, list)
            or len(entry) != 2
            or not all(isinstance(value, str) and value.strip() for value in entry)
        ):
            raise PilotContractError("remembered session has an invalid pilot hidden-check entry")
        segment_id, actual = entry
        reviewer = _canonical_reviewer(policy, actual, "remembered hidden-key cache")
        if segment_id != segment_id.strip():
            raise PilotContractError("remembered hidden-key cache contains a non-canonical segment ID")
        if segment_id in session_keys[reviewer]:
            raise PilotContractError(f"remembered hidden-key cache duplicates {reviewer}/{segment_id}")
        if segment_id not in grants[reviewer]:
            raise PilotContractError(
                f"remembered hidden-key cache contains unreserved key {reviewer}/{segment_id}"
            )
        session_keys[reviewer].add(segment_id)

    corpus_actions = {name: 0 for name in policy.reviewer_caps}
    hidden_actions = {name: 0 for name in policy.reviewer_caps}
    completed_keys = {name: set() for name in policy.reviewer_caps}
    skipped_keys = {name: set() for name in policy.reviewer_caps}
    hidden_event_actions: dict[tuple[str, str], str] = {}
    for event in history.effective_events:
        event_id = event.event_id
        segment_id = event.segment_id
        actual = event.reviewer
        action = event.action
        source = event.source
        reviewer = _canonical_reviewer(policy, actual, "controlled-review history")
        if source == "couch":
            if action not in {"accept", "edit", "reject", "skip"}:
                raise PilotContractError(f"post-baseline Couch event {event_id} has invalid action {action!r}")
            corpus_actions[reviewer] += 1
            if segment_id in grants[reviewer]:
                if action != "skip":
                    raise PilotContractError(
                        f"durable hidden key {reviewer}/{segment_id} was finalized through the corpus path"
                    )
                if segment_id in skipped_keys[reviewer]:
                    raise PilotContractError(f"hidden key {reviewer}/{segment_id} was skipped more than once")
                skipped_keys[reviewer].add(segment_id)
        else:
            if action not in {"accept", "edit", "reject", "skip"}:
                raise PilotContractError(f"hidden-check event {event_id} has invalid action {action!r}")
            if segment_id not in grants[reviewer]:
                raise PilotContractError(
                    f"hidden-check event {event_id} has no active durable reservation"
                )
            if segment_id in completed_keys[reviewer] or segment_id in skipped_keys[reviewer]:
                raise PilotContractError(f"hidden key {reviewer}/{segment_id} was resolved more than once")
            hidden_event_actions[(reviewer, segment_id)] = action
            if action == "skip":
                skipped_keys[reviewer].add(segment_id)
            else:
                completed_keys[reviewer].add(segment_id)
            hidden_actions[reviewer] += 1

    # ``spot_checks`` and its review event are committed in one Rust transaction.  Prove both halves
    # still exist and agree; otherwise runtime resolution and the release gate would count different
    # histories after corruption or an unsupported manual edit.
    try:
        result_rows = list(
            connection.execute(
                """SELECT key.reviewer, key.segment_id, result.action
                     FROM review_pilot_hidden_keys key
                     JOIN spot_checks result
                       ON result.segment_id = key.segment_id
                      AND result.reviewer = key.reviewer COLLATE NOCASE
                    WHERE key.policy_sha256 = ? AND key.after_review_event_id = ?
                    ORDER BY key.reviewer COLLATE NOCASE, key.segment_id""",
                (digest, baseline),
            )
        )
    except sqlite3.Error as error:
        raise PilotContractError(f"hidden-check result evidence cannot be read: {error}") from error
    result_actions: dict[tuple[str, str], list[str]] = {}
    for actual, segment_id, action in result_rows:
        reviewer = _canonical_reviewer(policy, actual, "hidden-check results")
        result_actions.setdefault((reviewer, str(segment_id)), []).append(str(action))
    for key, expected_action in hidden_event_actions.items():
        observed = result_actions.get(key, [])
        if observed != [expected_action]:
            raise PilotContractError(
                f"hidden key {key[0]}/{key[1]} event/result mismatch: event={expected_action!r}, results={observed!r}"
            )
    orphan_results = sorted(set(result_actions) - set(hidden_event_actions))
    if orphan_results:
        reviewer, segment_id = orphan_results[0]
        raise PilotContractError(
            f"hidden key {reviewer}/{segment_id} has a result without a post-baseline hidden event"
        )

    for reviewer in policy.reviewer_caps:
        overlap = completed_keys[reviewer] & skipped_keys[reviewer]
        if overlap:
            raise PilotContractError(
                f"hidden key {reviewer}/{sorted(overlap)[0]} has both a completed result and a skip"
            )
        if corpus_actions[reviewer] > policy.reviewer_caps[reviewer]:
            raise PilotContractError(f"controlled-review history exceeds the 10-action cap for {reviewer}")
        if hidden_actions[reviewer] > HIDDEN_KEYS_PER_REVIEWER:
            raise PilotContractError(f"hidden-check history exceeds the 2-action cap for {reviewer}")
        if corpus_actions[reviewer] + hidden_actions[reviewer] > (
            policy.reviewer_caps[reviewer] + HIDDEN_KEYS_PER_REVIEWER
        ):
            raise PilotContractError(f"controlled-review history exceeds the 12-UI-action cap for {reviewer}")
    if sum(corpus_actions.values()) > policy.max_total_corpus_actions:
        raise PilotContractError("controlled-review history exceeds the 20-action corpus limit")
    if sum(hidden_actions.values()) > TOTAL_HIDDEN_KEYS:
        raise PilotContractError("hidden-check history exceeds the 4-action global limit")
    if sum(corpus_actions.values()) + sum(hidden_actions.values()) > MAX_UI_ACTIONS:
        raise PilotContractError(f"controlled-review history exceeds the {MAX_UI_ACTIONS}-UI-action limit")

    # Files are not covered by SQLite's read transaction.  Refuse a composite verdict if either
    # operating file moved while the database snapshot was being audited.
    try:
        if session_path.read_text(encoding="utf-8") != session_raw:
            raise PilotContractError(f"{SESSION_FILE} changed during the hidden-key audit")
    except OSError as error:
        raise PilotContractError(f"{SESSION_FILE} cannot be rechecked: {error}") from error
    final_policy = read_policy(data_dir / POLICY_FILE)
    if final_policy != policy or policy_sha256(final_policy) != digest:
        raise PilotContractError(f"{POLICY_FILE} changed during the hidden-key audit")

    unresolved_keys = {
        reviewer: grants[reviewer] - completed_keys[reviewer] - skipped_keys[reviewer]
        for reviewer in policy.reviewer_caps
    }
    return HiddenPilotState(
        policy_sha256=digest,
        grants=grants,
        session_keys=session_keys,
        completed_keys=completed_keys,
        skipped_keys=skipped_keys,
        unresolved_keys=unresolved_keys,
        corpus_actions=corpus_actions,
        hidden_actions=hidden_actions,
        effective_events=history.effective_events,
        raw_original_count=history.raw_original_count,
        reversal_count=history.reversal_count,
    )
