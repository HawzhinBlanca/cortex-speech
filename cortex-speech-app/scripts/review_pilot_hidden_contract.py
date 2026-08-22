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
from dataclasses import dataclass
from pathlib import Path


REQUIRED_SCHEMA = 59
POLICY_FILE = "review_pilot_policy.json"
SESSION_FILE = "couch_session.json"
POLICY_SCHEMA_VERSION = 1
PILOT_REVIEWERS = ("Hawzhin", "Pavel")
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
class HiddenPilotState:
    policy_sha256: str
    grants: dict[str, set[str]]
    session_keys: dict[str, set[str]]
    completed_keys: dict[str, set[str]]
    skipped_keys: dict[str, set[str]]
    unresolved_keys: dict[str, set[str]]
    corpus_actions: dict[str, int]
    hidden_actions: dict[str, int]

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
            f"{source} must name exactly Hawzhin and Pavel at {CORPUS_ACTIONS_PER_REVIEWER} actions each"
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
    try:
        events = list(
            connection.execute(
                """SELECT id, segment_id, reviewer, action, source FROM review_events
                    WHERE id > ? AND source IN ('couch', 'couch_spot_check')
                    ORDER BY id""",
                (baseline,),
            )
        )
    except sqlite3.Error as error:
        raise PilotContractError(f"post-baseline review history cannot be read: {error}") from error
    for event_id, segment_id, actual, action, source in events:
        reviewer = _canonical_reviewer(policy, actual, "controlled-review history")
        action = str(action)
        source = str(source)
        segment_id = str(segment_id)
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
    )
