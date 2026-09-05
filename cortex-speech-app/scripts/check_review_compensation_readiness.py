#!/usr/bin/env python3
"""Fail-closed production gate for Cortex reviewer compensation in the active review mode.

In legacy controlled-pilot mode this gate reads the migrated live database and active voice focus
without modifying either. It proves the authorized policy constants, one ledger consequence per
effective post-cutoff event, append-only triggers, durable schema-v60 hidden-key grants, the
24-action pilot ceiling, signed re-decision/reversal arithmetic, and canonical audio identity for
every focused clip.

In flexible-pool mode (owner canon 2026-09-04: pool second opinions are paid at the first-opinion
weights) the gate audits the real ledger fail-closed on the exact current schema: every post-cutoff
first opinion and every non-skip pool opinion carries exactly one exact credit, every pool credit its
consumed policy-4 listening authority, every undo its durable reversal, every settlement its exact
contiguous ledger range. A source migration or a green boolean alone is never accepted as live
evidence.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import uuid
from pathlib import Path
from typing import Any

from check_spot_check_pool import PolicyBroken, active_flexible_pool
from pilot_focus_contract import (
    CANONICAL_IDENTITY_KIND,
    PLAYBACK_GUARD_VERSION,
    canonical_audio_work_id,
    canonical_reviewer_work_id,
    canonical_source_span,
    source_span_duration_issue,
)

from review_pilot_hidden_contract import (
    COMPENSATION_POLICY_VERSION,
    POLICY_FILE as REVIEW_PILOT_FILE,
    REQUIRED_SCHEMA as REVIEW_PILOT_REQUIRED_SCHEMA,
    PilotContractError,
    audit_active_hidden_state,
    audit_hidden_schema,
    read_policy,
)


POLICY_VERSION = COMPENSATION_POLICY_VERSION
BASE_RATE_MICRO_IQD_PER_HOUR = 18_000_000_000
BASIS_POINTS = {"edit": 10_000, "accept": 1_000, "reject": 1_000, "skip": 0}
# The flexible paid audit reads schema-specific evidence tables (policy-4 sessions/receipts/consumptions,
# pool decisions/reversals) and is pinned to the exact deployed schema on purpose: a migration must
# re-earn this gate, never inherit it.
FLEXIBLE_PAID_SCHEMA_VERSIONS = (69, 70)
# Mirrors src-tauri/src/db/core.rs MIN_PLAYBACK_COVERAGE / DESKTOP_PLAYBACK_POLICY_VERSION.
MIN_PLAYBACK_COVERAGE = 0.85
COUCH_PLAYBACK_POLICY_VERSION = 4
REQUIRED_TRIGGERS = {
    "review_event_operation_validate_insert",
    "review_event_operation_immutable_update",
    "review_compensation_policy_immutable_update",
    "review_compensation_policy_immutable_delete",
    "review_compensation_ledger_immutable_update",
    "review_compensation_ledger_immutable_delete",
    "review_compensation_settlement_validate_insert",
    "review_compensation_settlement_immutable_update",
    "review_compensation_settlement_immutable_delete",
    "review_events_v60_provenance_validate_insert",
    "review_events_v60_provenance_immutable_update",
    "review_events_v60_post_cutoff_immutable_update",
    "review_events_v60_post_cutoff_immutable_delete",
    "review_effect_state_immutable_insert",
    "review_effect_state_immutable_update",
    "review_effect_state_immutable_delete",
    "human_decision_effect_events_validate_review_event_insert",
    "human_decision_effect_events_immutable_update",
    "human_decision_effect_events_immutable_delete",
    "human_decision_effect_reversals_validate_phone_insert",
    "human_decision_effect_reversals_immutable_update",
    "human_decision_effect_reversals_immutable_delete",
}


# Second-pass judging commits to its own append-only tables and never touches review_events, so the
# ledger has nothing to price and no credit ever appears.  The ledger is not wrong — it paid exactly
# what it was told to pay — the work is simply unrecorded, and the reviewer's phone shows a flat
# balance with no sign of it.  Every entry is (table, its reversal table); the effective row set is
# the decision minus its reversal, exactly as effective_review_pool_decisions_v62 /
# effective_independent_review_decisions_v61 define it.
# The third element is the SQL predicate (over aliases `ledger` and `decision`) that links a credit to
# the decision. Pool credits are keyed `pool-decision:<id>` with source `couch_pool` (owner canon
# 2026-09-04); the legacy shared-operation-id link stays recognised so history is never re-flagged.
UNPAID_DECISION_TABLES = (
    (
        "review_pool_decisions",
        "review_pool_reversals",
        "(ledger.source = 'couch_pool' AND ledger.entry_key = 'pool-decision:' || decision.id)"
        " OR EXISTS (SELECT 1 FROM review_events event"
        "             WHERE event.id = ledger.review_event_id AND event.operation_id = decision.operation_id)",
    ),
    (
        "independent_review_decisions",
        "independent_review_reversals",
        "EXISTS (SELECT 1 FROM review_events event"
        "         WHERE event.id = ledger.review_event_id AND event.operation_id = decision.operation_id)",
    ),
)
# skip carries 0 basis points under review-iqd-v1-2026-08-21, so an uncredited skip is not unpaid
# work; counting it would inflate the very number the owner has to decide on.
PAYABLE_WEIGHT_ACTIONS = ("accept", "edit", "reject")


def _default_data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if not appdata:
        raise RuntimeError("APPDATA is not set; pass --data-dir explicitly")
    return Path(appdata) / "cortex-speech"


def _connect_read_only(path: Path) -> sqlite3.Connection:
    uri = f"file:{path.resolve().as_posix()}?mode=ro"
    connection = sqlite3.connect(uri, uri=True)
    connection.row_factory = sqlite3.Row
    # One gate run must describe one SQLite snapshot. Without an explicit read transaction, each
    # SELECT can observe a different commit while reviewers are landing decisions, yielding a
    # composite report that never existed at any instant.
    connection.execute("PRAGMA query_only = ON")
    connection.execute("BEGIN")
    return connection


def _audit_settlements(
    connection: sqlite3.Connection, ledger_rows: list[sqlite3.Row], errors: list[str]
) -> dict[str, Any]:
    """Every settlement covers one exact, contiguous, unique-referenced range of the reviewer's ledger."""
    settlements = list(
        connection.execute(
            """SELECT settlement_id, reviewer, from_ledger_id_exclusive,
                      through_ledger_id_inclusive, allocated_micro_iqd, payout_reference
                 FROM review_compensation_settlements
                WHERE policy_version = ?
                ORDER BY reviewer COLLATE NOCASE, through_ledger_id_inclusive""",
            (POLICY_VERSION,),
        )
    )
    last_boundary: dict[str, int] = {}
    payout_references: set[str] = set()
    for settlement in settlements:
        reviewer_key = str(settlement["reviewer"]).strip().casefold()
        expected_from = last_boundary.get(reviewer_key, 0)
        observed_from = int(settlement["from_ledger_id_exclusive"])
        through = int(settlement["through_ledger_id_inclusive"])
        if observed_from != expected_from or through <= observed_from:
            errors.append(f"settlement {settlement['settlement_id']} has a non-contiguous range")
        exact = sum(
            int(row["delta_micro_iqd"])
            for row in ledger_rows
            if str(row["reviewer"]).strip().casefold() == reviewer_key
            and int(row["id"]) > observed_from
            and int(row["id"]) <= through
        )
        if exact != int(settlement["allocated_micro_iqd"]):
            errors.append(f"settlement {settlement['settlement_id']} amount differs from immutable ledger range")
        reference = str(settlement["payout_reference"]).strip()
        if not reference or reference in payout_references:
            errors.append(f"settlement {settlement['settlement_id']} has an empty/duplicate payout reference")
        payout_references.add(reference)
        last_boundary[reviewer_key] = through
    return {
        "settlements": len(settlements),
        "settledMicroIqd": sum(int(row["allocated_micro_iqd"]) for row in settlements),
    }


def _check_credit_arithmetic(
    row: sqlite3.Row, action: str, prior: int, prior_corrected: int, errors: list[str]
) -> None:
    """Rate, entitlement, delta and corrected-audio math of one credit against the running balances."""
    entry_id = str(row["entry_id"])
    expected_bps = BASIS_POINTS.get(action)
    if expected_bps is None:
        errors.append(f"ledger entry {entry_id} has unsupported action {action!r}")
        return
    try:
        entitlement = _exact_entitlement(int(row["duration_ms"]), expected_bps) if expected_bps else 0
    except ValueError as error:
        errors.append(f"ledger entry {entry_id}: {error}")
        entitlement = -1
    if int(row["rate_basis_points"]) != expected_bps or int(row["entitlement_micro_iqd"]) != entitlement:
        errors.append(f"ledger rate/entitlement mismatch at {entry_id}")
    expected_delta = 0 if action == "skip" else entitlement - prior
    if int(row["delta_micro_iqd"]) != expected_delta:
        errors.append(f"ledger delta mismatch at {entry_id}: {row['delta_micro_iqd']} != {expected_delta}")
    target_corrected = int(row["duration_ms"]) if action == "edit" else prior_corrected if action == "skip" else 0
    if int(row["corrected_entitlement_ms"]) != target_corrected:
        errors.append(
            f"corrected entitlement mismatch at {entry_id}: {row['corrected_entitlement_ms']} != {target_corrected}"
        )
    if int(row["delta_corrected_ms"]) != target_corrected - prior_corrected:
        errors.append(
            f"corrected delta mismatch at {entry_id}: {row['delta_corrected_ms']} != {target_corrected - prior_corrected}"
        )


def _expected_reviewer_work_id(connection: sqlite3.Connection, reviewer: object, segment_id: object) -> tuple[str | None, str]:
    segment = connection.execute(
        "SELECT id, audio_content_hash, alignment_json, duration_ms FROM speech_segments WHERE id = ?",
        (segment_id,),
    ).fetchone()
    if segment is None:
        return None, "segment is missing"
    canonical, identity_reason, _audio_work_id = _canonical_focus_status(segment)
    if not canonical:
        return None, identity_reason
    return canonical_reviewer_work_id(reviewer, segment["audio_content_hash"], segment["alignment_json"])


def audit_flexible_paid(db_path: Path) -> dict[str, Any] | None:
    """Return the flexible-pool PAID compensation audit, or None when no pool is active.

    Owner canon 2026-09-04: pool second opinions are paid at the first-opinion weights (edit 100%,
    accept 10%, reject 10%). This mode therefore audits the real ledger instead of proving that
    nothing was paid: one exact credit per post-cutoff first opinion and per non-skip pool opinion,
    consumed policy-4 listening behind every pool credit, a durable reversal behind every undo, exact
    contiguous settlements. It reads schema-specific tables and is pinned to the exact deployed schema.
    """
    if not db_path.is_file():
        return None
    try:
        connection = _connect_read_only(db_path)
    except sqlite3.Error as error:
        return {
            "database": str(db_path.resolve()),
            "mode": "unknown",
            "ok": False,
            "errors": [f"cannot open database read-only: {error}"],
        }
    errors: list[str] = []
    evidence: dict[str, Any] = {
        "database": str(db_path.resolve()),
        "mode": "flexible-pool",
        "compensationOperationalStatus": "paid",
        "policyVersion": POLICY_VERSION,
        "uncreditedSecondPassDecisions": [],
        "warnings": [],
    }
    try:
        pool = active_flexible_pool(connection)
        if pool is None:
            return None
        pool_id, member_count, focus_sha256 = pool
        evidence.update({"poolId": pool_id, "poolMembers": member_count, "poolFocusSha256": focus_sha256})
        if (db_path.parent / REVIEW_PILOT_FILE).exists():
            errors.append("flexible pool and legacy controlled-pilot policy are active together")

        schema_version = int(
            connection.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
        )
        evidence["schemaVersion"] = schema_version
        if schema_version not in FLEXIBLE_PAID_SCHEMA_VERSIONS:
            errors.append(
                f"flexible paid-compensation audit requires exact schema {FLEXIBLE_PAID_SCHEMA_VERSIONS}, "
                f"found {schema_version}"
            )
            return {**evidence, "ok": False, "errors": errors}

        policy_rows = connection.execute(
            """SELECT effective_after_event_id, base_rate_micro_iqd_per_hour,
                      edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
                 FROM review_compensation_policies WHERE policy_version = ?""",
            (POLICY_VERSION,),
        ).fetchall()
        evidence["policyRows"] = len(policy_rows)
        if len(policy_rows) != 1:
            errors.append(f"expected one immutable {POLICY_VERSION} policy row, found {len(policy_rows)}")
            return {**evidence, "ok": False, "errors": errors}
        policy = policy_rows[0]
        observed = (
            policy["base_rate_micro_iqd_per_hour"],
            policy["edit_basis_points"],
            policy["accept_basis_points"],
            policy["reject_basis_points"],
            policy["skip_basis_points"],
        )
        expected = (BASE_RATE_MICRO_IQD_PER_HOUR, 10_000, 1_000, 1_000, 0)
        if observed != expected:
            errors.append(f"policy constants differ: observed={observed}, expected={expected}")
        cutoff = int(policy["effective_after_event_id"])
        evidence["effectiveAfterEventId"] = cutoff

        triggers = {
            row["name"]: row["sql"] or ""
            for row in connection.execute(
                """SELECT name, sql FROM sqlite_master
                    WHERE type = 'trigger'
                      AND (name LIKE 'review_compensation_%'
                           OR name LIKE 'review_event_operation_%'
                           OR name LIKE 'review_events_v60_%'
                           OR name LIKE 'review_effect_state_%'
                           OR name LIKE 'human_decision_effect_%')"""
            )
        }
        missing_triggers = sorted(REQUIRED_TRIGGERS - triggers.keys())
        if missing_triggers:
            errors.append(f"missing immutable triggers: {missing_triggers}")
        non_aborting = sorted(
            name for name in REQUIRED_TRIGGERS & triggers.keys() if "RAISE(ABORT" not in triggers[name].upper()
        )
        if non_aborting:
            errors.append(f"immutable triggers do not abort writes: {non_aborting}")
        evidence["immutableTriggers"] = sorted(triggers)

        operation_collisions = int(
            connection.execute(
                """SELECT COUNT(*) FROM review_pool_decisions pool
                    JOIN review_events event ON event.operation_id=pool.operation_id"""
            ).fetchone()[0]
        )
        evidence["crossNamespaceOperationCollisions"] = operation_collisions
        if operation_collisions:
            errors.append(f"{operation_collisions} operation UUID(s) exist in both pool and legacy review namespaces")

        uncredited, uncredited_warnings = _uncredited_second_pass_decisions(connection)
        evidence["uncreditedSecondPassDecisions"] = uncredited
        evidence["warnings"] = uncredited_warnings

        events = {
            int(row["id"]): row
            for row in connection.execute(
                """SELECT id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                          operation_id, operation_payload_hash
                     FROM review_events
                    WHERE id > ? AND source IN ('couch', 'couch_spot_check')
                    ORDER BY id""",
                (cutoff,),
            )
        }
        all_ledger_rows = list(
            connection.execute(
                """SELECT id, entry_id, entry_key, source, policy_version, review_event_id,
                          canonical_work_id, canonical_identity_kind, reviewer, segment_id,
                          compensation_action, effective_decision, decision_revision, duration_ms,
                          rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                          corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id
                     FROM review_compensation_ledger ORDER BY id"""
            )
        )
        ledger_rows = [row for row in all_ledger_rows if row["policy_version"] == POLICY_VERSION]
        foreign_policy_rows = [row for row in all_ledger_rows if row["policy_version"] != POLICY_VERSION]
        evidence["rawPostCutoffEvents"] = len(events)
        evidence["ledgerEntries"] = len(ledger_rows)
        evidence["allPolicyLedgerEntries"] = len(all_ledger_rows)
        for row in foreign_policy_rows:
            if row["review_event_id"] is not None and int(row["review_event_id"]) in events:
                errors.append(
                    f"post-cutoff event {row['review_event_id']} has a foreign-policy ledger consequence "
                    f"{row['policy_version']!r}"
                )

        event_entry_counts: dict[int, int] = {}
        pool_entry_counts: dict[int, int] = {}
        entry_by_id: dict[str, sqlite3.Row] = {}
        reversed_entries: set[str] = set()
        balances: dict[str, int] = {}
        corrected_balances: dict[str, int] = {}
        for row in ledger_rows:
            entry_id = str(row["entry_id"])
            work_id = str(row["canonical_work_id"])
            prior = balances.get(work_id, 0)
            prior_corrected = corrected_balances.get(work_id, 0)
            action = str(row["compensation_action"])
            source = str(row["source"])
            entry_key = str(row["entry_key"])
            if row["canonical_identity_kind"] != CANONICAL_IDENTITY_KIND:
                errors.append(f"ledger entry {entry_id} does not carry the canonical audio identity kind")

            if row["review_event_id"] is not None:
                review_event_id = int(row["review_event_id"])
                event_entry_counts[review_event_id] = event_entry_counts.get(review_event_id, 0) + 1
                event = events.get(review_event_id)
                if event is None:
                    errors.append(f"ledger entry {entry_id} points outside the post-cutoff event range")
                else:
                    if (
                        action != str(event["compensation_action"] or "")
                        or row["effective_decision"] != event["action"]
                        or source != event["source"]
                        or entry_key != f"review-event:{review_event_id}"
                        or row["reverses_entry_id"] is not None
                    ):
                        errors.append(f"ledger entry {entry_id} disagrees with review event {review_event_id}")
                    if (
                        row["segment_id"] != event["segment_id"]
                        or str(row["reviewer"]).casefold() != str(event["reviewer"]).casefold()
                    ):
                        errors.append(f"ledger identity {entry_id} disagrees with review event {review_event_id}")
                    if int(row["duration_ms"]) != int(event["duration_ms"]):
                        errors.append(f"ledger duration {entry_id} disagrees with review event {review_event_id}")
                    expected_work_id, identity_reason = _expected_reviewer_work_id(
                        connection, event["reviewer"], event["segment_id"]
                    )
                    if expected_work_id is None or work_id != expected_work_id:
                        errors.append(
                            f"ledger canonical identity {entry_id} disagrees with event segment: "
                            f"expected work={expected_work_id!r}; reason={identity_reason or 'none'}"
                        )
                _check_credit_arithmetic(row, action, prior, prior_corrected, errors)
            elif source == "couch_pool":
                # Owner canon 2026-09-04: a pool second opinion is paid like a first opinion. It has no
                # review_events row; its provenance is the immutable review_pool_decisions row named by
                # the entry key and its listening proof the `independent` policy-4 consumption bound to
                # that row's operation id, session and receipt.
                try:
                    pool_decision_id = int(entry_key.split(":", 1)[1]) if entry_key.startswith("pool-decision:") else -1
                except ValueError:
                    pool_decision_id = -1
                pool_entry_counts[pool_decision_id] = pool_entry_counts.get(pool_decision_id, 0) + 1
                decision = connection.execute(
                    """SELECT segment_id, reviewer, action, served_revision, duration_ms,
                              audio_content_hash, source_start_ms, source_end_ms, operation_id
                         FROM review_pool_decisions WHERE id = ?""",
                    (pool_decision_id,),
                ).fetchone()
                if decision is None:
                    errors.append(f"pool credit {entry_id} names a missing pool decision {pool_decision_id}")
                else:
                    if (
                        str(decision["action"]) == "skip"
                        or action != str(decision["action"])
                        or row["effective_decision"] != decision["action"]
                        or row["reverses_entry_id"] is not None
                    ):
                        errors.append(f"pool credit {entry_id} disagrees with pool decision {pool_decision_id}")
                    if (
                        row["segment_id"] != decision["segment_id"]
                        or str(row["reviewer"]).casefold() != str(decision["reviewer"]).casefold()
                    ):
                        errors.append(f"pool credit identity {entry_id} disagrees with pool decision {pool_decision_id}")
                    if int(row["duration_ms"]) != int(decision["duration_ms"]):
                        errors.append(f"pool credit duration {entry_id} disagrees with pool decision {pool_decision_id}")
                    if row["decision_revision"] != decision["served_revision"]:
                        errors.append(f"pool credit revision {entry_id} disagrees with pool decision {pool_decision_id}")
                    expected_work_id, identity_reason = _expected_reviewer_work_id(
                        connection, decision["reviewer"], decision["segment_id"]
                    )
                    if expected_work_id is None or work_id != expected_work_id:
                        errors.append(
                            f"pool credit canonical identity {entry_id} disagrees with its segment: "
                            f"expected work={expected_work_id!r}; reason={identity_reason or 'none'}"
                        )
                    span, _span_reason = canonical_source_span(
                        connection.execute(
                            "SELECT alignment_json FROM speech_segments WHERE id = ?", (decision["segment_id"],)
                        ).fetchone()[0]
                        if expected_work_id is not None
                        else None
                    )
                    if span is None or (decision["source_start_ms"], decision["source_end_ms"]) != span:
                        errors.append(f"pool credit {entry_id} was paid for audio other than decision {pool_decision_id} judged")
                    listening = int(
                        connection.execute(
                            """SELECT COUNT(*)
                                 FROM playback_receipts receipt
                                 JOIN desktop_playback_sessions_v4 session
                                   ON session.playback_receipt_id = receipt.authority_session_id
                                  AND session.surface = 'couch'
                                 JOIN playback_authority_consumptions_v4 consumption
                                   ON consumption.playback_receipt_id = receipt.authority_session_id
                                WHERE receipt.policy_version = ?
                                  AND receipt.segment_id = ?
                                  AND receipt.reviewer = ? COLLATE NOCASE
                                  AND receipt.segment_revision = ?
                                  AND receipt.audio_fingerprint = ?
                                  AND receipt.source_start_ms = ?
                                  AND receipt.source_end_ms = ?
                                  AND receipt.clip_duration_ms = ?
                                  AND receipt.coverage_ratio >= ?
                                  AND session.reviewer = ? COLLATE NOCASE
                                  AND session.segment_id = ?
                                  AND session.segment_revision = ?
                                  AND session.audio_content_hash = ?
                                  AND consumption.namespace = 'independent'
                                  AND consumption.operation_id = ?
                                  AND consumption.reviewer = ? COLLATE NOCASE
                                  AND consumption.segment_id = ?""",
                            (
                                COUCH_PLAYBACK_POLICY_VERSION,
                                decision["segment_id"],
                                decision["reviewer"],
                                decision["served_revision"],
                                decision["audio_content_hash"],
                                decision["source_start_ms"],
                                decision["source_end_ms"],
                                decision["duration_ms"],
                                MIN_PLAYBACK_COVERAGE,
                                decision["reviewer"],
                                decision["segment_id"],
                                decision["served_revision"],
                                decision["audio_content_hash"],
                                decision["operation_id"],
                                decision["reviewer"],
                                decision["segment_id"],
                            ),
                        ).fetchone()[0]
                    )
                    if listening == 0:
                        errors.append(
                            f"pool credit {entry_id} has no exact consumed policy-4 playback authority for decision {pool_decision_id}"
                        )
                _check_credit_arithmetic(row, action, prior, prior_corrected, errors)
            elif action == "undo":
                reverses = row["reverses_entry_id"]
                target = entry_by_id.get(str(reverses)) if reverses is not None else None
                if target is None:
                    errors.append(f"undo {entry_id} references a missing/later entry {reverses}")
                    expected_delta = 0
                    expected_corrected_delta = 0
                else:
                    expected_delta = -int(target["delta_micro_iqd"])
                    expected_corrected_delta = -int(target["delta_corrected_ms"])
                    if (
                        str(target["compensation_action"]) == "undo"
                        or str(target["canonical_work_id"]) != work_id
                        or target["segment_id"] != row["segment_id"]
                        or str(target["reviewer"]).casefold() != str(row["reviewer"]).casefold()
                        or int(target["duration_ms"]) != int(row["duration_ms"])
                        or target["decision_revision"] != row["decision_revision"]
                        or str(reverses) in reversed_entries
                    ):
                        errors.append(f"undo {entry_id} does not exactly bind its earlier decision entry")
                    reversed_entries.add(str(reverses))
                    undo_operation = entry_key[len("undo:"):] if entry_key.startswith("undo:") else ""
                    if str(target["source"]) == "couch_pool":
                        target_key = str(target["entry_key"])
                        try:
                            target_decision_id = int(target_key.split(":", 1)[1])
                        except (IndexError, ValueError):
                            target_decision_id = -1
                        reversal_rows = int(
                            connection.execute(
                                """SELECT COUNT(*) FROM review_pool_reversals
                                    WHERE decision_id = ? AND operation_id = ? AND reviewer = ? COLLATE NOCASE""",
                                (target_decision_id, undo_operation, row["reviewer"]),
                            ).fetchone()[0]
                        )
                        if source != "couch_pool_undo" or reversal_rows != 1:
                            errors.append(f"undo {entry_id} does not name the durable pool reversal it settles")
                    elif source != "couch_undo":
                        errors.append(f"undo {entry_id} of a first-opinion credit must be a couch_undo entry")
                if row["effective_decision"] != "undo" or int(row["rate_basis_points"]) != 0 or int(row["entitlement_micro_iqd"]) != 0:
                    errors.append(f"undo {entry_id} has invalid fixed semantics")
                if int(row["delta_micro_iqd"]) != expected_delta:
                    errors.append(f"undo delta mismatch at {entry_id}: {row['delta_micro_iqd']} != {expected_delta}")
                if int(row["delta_corrected_ms"]) != expected_corrected_delta:
                    errors.append(
                        f"undo corrected delta mismatch at {entry_id}: "
                        f"{row['delta_corrected_ms']} != {expected_corrected_delta}"
                    )
                if int(row["corrected_entitlement_ms"]) != prior_corrected + expected_corrected_delta:
                    errors.append(
                        f"undo corrected entitlement mismatch at {entry_id}: "
                        f"{row['corrected_entitlement_ms']} != {prior_corrected + expected_corrected_delta}"
                    )
            else:
                errors.append(f"ledger entry {entry_id} has neither review event, pool decision nor undo semantics")

            entry_by_id[entry_id] = row
            balances[work_id] = prior + int(row["delta_micro_iqd"])
            corrected_balances[work_id] = prior_corrected + int(row["delta_corrected_ms"])
            if balances[work_id] < 0:
                errors.append(f"canonical work {work_id} has a negative running entitlement")
            if corrected_balances[work_id] < 0:
                errors.append(f"canonical work {work_id} has a negative corrected-audio entitlement")

        for event_id, event in events.items():
            if str(event["compensation_action"] or "") not in BASIS_POINTS:
                errors.append(f"post-cutoff event {event_id} lacks a valid compensation_action")
            if event_entry_counts.get(event_id, 0) != 1:
                errors.append(f"post-cutoff event {event_id} has {event_entry_counts.get(event_id, 0)} ledger entries")
        paid_pool_decisions = connection.execute(
            "SELECT id, action FROM review_pool_decisions WHERE action <> 'skip' ORDER BY id"
        ).fetchall()
        for pool_decision in paid_pool_decisions:
            credits = pool_entry_counts.get(int(pool_decision["id"]), 0)
            if credits != 1:
                errors.append(
                    f"pool decision {pool_decision['id']} ({pool_decision['action']}) has {credits} ledger credits; "
                    "required exactly one"
                )
        evidence["postCutoffEvents"] = len(events)
        evidence["paidPoolDecisions"] = len(paid_pool_decisions)
        evidence["poolDecisionCredits"] = sum(pool_entry_counts.values())
        evidence["reversalEntries"] = len(reversed_entries)

        evidence.update(_audit_settlements(connection, ledger_rows, errors))

        fk_violations = 0
        for table in (
            "review_compensation_ledger",
            "review_compensation_policies",
            "review_compensation_settlements",
        ):
            fk_violations += len(list(connection.execute(f"PRAGMA foreign_key_check({table})")))
        evidence["compensationForeignKeyViolations"] = fk_violations
        if fk_violations:
            errors.append(f"compensation tables have {fk_violations} foreign-key violation(s)")

        evidence["totalEarnedMicroIqd"] = sum(int(row["delta_micro_iqd"]) for row in ledger_rows)
        evidence["correctedAudioMs"] = sum(corrected_balances.values())
        evidence["activeWorkBalances"] = sum(1 for value in balances.values() if value)
    except (sqlite3.Error, PolicyBroken, TypeError, ValueError) as error:
        errors.append(f"flexible paid-compensation authority cannot be proved: {error}")
    finally:
        connection.close()
    return {**evidence, "ok": not errors, "errors": errors}


def _exact_entitlement(duration_ms: int, basis_points: int) -> int:
    numerator = duration_ms * BASE_RATE_MICRO_IQD_PER_HOUR * basis_points
    denominator = 3_600_000 * 10_000
    if duration_ms <= 0 or numerator % denominator:
        raise ValueError("duration/rate combination does not yield exact positive micro-IQD")
    return numerator // denominator


def _canonical_focus_status(row: sqlite3.Row) -> tuple[bool, str, str | None]:
    if type(row["duration_ms"]) is not int or row["duration_ms"] <= 0:
        return False, "non-positive duration", None
    work_id, reason = canonical_audio_work_id(row["audio_content_hash"], row["alignment_json"])
    if work_id is None:
        return False, reason, None
    source_span, span_reason = canonical_source_span(row["alignment_json"])
    if source_span is None:
        return False, span_reason or "source span is invalid", None
    duration_issue = source_span_duration_issue(
        row["duration_ms"], source_span, subject="payable duration"
    )
    if duration_issue:
        return False, duration_issue, None
    return True, reason, work_id


def _uncredited_second_pass_decisions(
    connection: sqlite3.Connection,
) -> tuple[list[dict[str, Any]], list[str]]:
    """Count durable, playback-evidenced second-pass decisions that carry no ledger credit.

    Reported, never failed here.  Pool second opinions are paid since owner canon 2026-09-04 and the
    flexible paid audit fails closed on any unpaid non-skip pool decision; the blinded second pass
    (independent_review_decisions) is still unpriced, and this gate may not mint, backfill, or reprice
    a single micro-IQD, so that work is surfaced as a warning rather than a failure with no
    canon-legal remedy.

    A pool credit is keyed on its decision (`pool-decision:<id>`, owner canon 2026-09-04); the legacy
    shared-operation-id linkage is still recognised. A decision reversed by its append-only reversal
    table is not outstanding work.
    """
    tables = {
        str(row["name"]) for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    uncredited: list[dict[str, Any]] = []
    warnings: list[str] = []
    for table, reversals, credit_link in UNPAID_DECISION_TABLES:
        if table not in tables or reversals not in tables:
            continue
        placeholders = ",".join("?" for _ in PAYABLE_WEIGHT_ACTIONS)
        grouped = connection.execute(
            f"""SELECT lower(trim(decision.reviewer)) AS reviewer, COUNT(*) AS decisions,
                       COALESCE(SUM(decision.duration_ms), 0) AS duration_ms
                  FROM {table} decision
                 WHERE decision.action IN ({placeholders})
                   AND decision.playback_guard_version = ?
                   AND NOT EXISTS (SELECT 1 FROM {reversals} reversal
                                    WHERE reversal.decision_id = decision.id)
                   AND NOT EXISTS (SELECT 1 FROM review_compensation_ledger ledger
                                    WHERE {credit_link})
                 GROUP BY 1
                 ORDER BY 1""",
            (*PAYABLE_WEIGHT_ACTIONS, PLAYBACK_GUARD_VERSION),
        ).fetchall()
        if not grouped:
            continue
        decisions = sum(int(row["decisions"]) for row in grouped)
        duration_ms = sum(int(row["duration_ms"]) for row in grouped)
        uncredited.extend(
            {
                "table": table,
                "reviewer": str(row["reviewer"]),
                "decisions": int(row["decisions"]),
                "durationMs": int(row["duration_ms"]),
            }
            for row in grouped
        )
        per_reviewer = ", ".join(f"{row['reviewer']}={row['decisions']}" for row in grouped)
        warnings.append(
            f"UNPAID WORK: {decisions} playback-evidenced decisions on {table} carry no ledger "
            f"credit (owner decision pending: pay or declare unpaid); {duration_ms} ms across "
            f"{len(grouped)} reviewers [{per_reviewer}]"
        )
    return uncredited, warnings


def audit(db_path: Path, focus_path: Path) -> dict[str, Any]:
    errors: list[str] = []
    evidence: dict[str, Any] = {
        "database": str(db_path.resolve()),
        "focus": str(focus_path.resolve()),
        "policyVersion": POLICY_VERSION,
        "uncreditedSecondPassDecisions": [],
        "warnings": [],
    }
    if not db_path.is_file():
        return {**evidence, "ok": False, "errors": [f"database not found: {db_path}"]}
    if not focus_path.is_file():
        return {**evidence, "ok": False, "errors": [f"focus file not found: {focus_path}"]}

    try:
        focus_doc = json.loads(focus_path.read_text(encoding="utf-8"))
        focus_ids = focus_doc.get("segment_ids") if isinstance(focus_doc, dict) else None
        if not isinstance(focus_ids, list) or not all(isinstance(value, str) and value for value in focus_ids):
            raise ValueError("segment_ids must be a list of non-empty strings")
        if len(focus_ids) != len(set(focus_ids)):
            raise ValueError("segment_ids contains duplicates")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return {**evidence, "ok": False, "errors": [f"invalid focus file: {error}"]}
    evidence["focusIds"] = len(focus_ids)
    focus_id_set = set(focus_ids)

    try:
        pilot_policy = read_policy(db_path.parent / REVIEW_PILOT_FILE)
    except PilotContractError as error:
        return {**evidence, "ok": False, "errors": [f"controlled-review policy is invalid: {error}"]}

    try:
        connection = _connect_read_only(db_path)
    except sqlite3.Error as error:
        return {**evidence, "ok": False, "errors": [f"cannot open database read-only: {error}"]}

    with connection:
        # Scanned before the schema gate below can return early: the pool/blinded decision tables
        # only exist from migrations 61 and 62, so on every database new enough to hold unpaid
        # second-pass work the exact-schema-60 check bails out first and this section would never
        # run where it matters most.
        uncredited, uncredited_warnings = _uncredited_second_pass_decisions(connection)
        evidence["uncreditedSecondPassDecisions"] = uncredited
        evidence["warnings"] = uncredited_warnings

        try:
            schema_version = int(
                connection.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
            )
        except sqlite3.Error:
            schema_version = 0
        evidence["schemaVersion"] = schema_version
        if schema_version != REVIEW_PILOT_REQUIRED_SCHEMA:
            errors.append(
                f"schema {schema_version} is not exact required pilot schema {REVIEW_PILOT_REQUIRED_SCHEMA}"
            )
            return {**evidence, "ok": False, "errors": errors}

        hidden_schema_evidence, hidden_schema_errors = audit_hidden_schema(connection)
        evidence.update(hidden_schema_evidence)
        errors.extend(hidden_schema_errors)
        if hidden_schema_errors:
            return {**evidence, "ok": False, "errors": errors}
        try:
            hidden_state = audit_active_hidden_state(connection, db_path.parent, db_path, pilot_policy)
        except PilotContractError as error:
            errors.append(f"controlled-review hidden-key state is invalid: {error}")
            return {**evidence, "ok": False, "errors": errors}
        evidence.update(
            {
                "pilotPolicySha256": hidden_state.policy_sha256,
                "pilotCorpusActions": hidden_state.total_corpus_actions,
                "pilotHiddenActions": hidden_state.total_hidden_actions,
                "pilotUiActions": hidden_state.total_ui_actions,
                "pilotHiddenGrants": sum(len(ids) for ids in hidden_state.grants.values()),
                "pilotHiddenResolved": sum(
                    len(hidden_state.completed_keys[name] | hidden_state.skipped_keys[name])
                    for name in hidden_state.grants
                ),
                "pilotHiddenUnresolved": sum(len(ids) for ids in hidden_state.unresolved_keys.values()),
            }
        )

        policy_rows = connection.execute(
            """SELECT effective_after_event_id, base_rate_micro_iqd_per_hour,
                      edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
                 FROM review_compensation_policies WHERE policy_version = ?""",
            (POLICY_VERSION,),
        ).fetchall()
        if len(policy_rows) != 1:
            errors.append(f"expected one {POLICY_VERSION} policy row, found {len(policy_rows)}")
            return {**evidence, "ok": False, "errors": errors}
        policy = policy_rows[0]
        cutoff = int(policy["effective_after_event_id"])
        evidence["effectiveAfterEventId"] = cutoff
        observed_policy = (
            int(policy["base_rate_micro_iqd_per_hour"]),
            int(policy["edit_basis_points"]),
            int(policy["accept_basis_points"]),
            int(policy["reject_basis_points"]),
            int(policy["skip_basis_points"]),
        )
        expected_policy = (BASE_RATE_MICRO_IQD_PER_HOUR, 10_000, 1_000, 1_000, 0)
        if observed_policy != expected_policy:
            errors.append(f"policy constants differ: observed={observed_policy}, expected={expected_policy}")

        triggers = {
            row["name"]: row["sql"] or ""
            for row in connection.execute(
                """SELECT name, sql FROM sqlite_master
                    WHERE type = 'trigger'
                      AND (name LIKE 'review_compensation_%'
                           OR name LIKE 'review_event_operation_%'
                           OR name LIKE 'review_events_v60_%'
                           OR name LIKE 'review_effect_state_%'
                           OR name LIKE 'human_decision_effect_%')"""
            )
        }
        missing_triggers = sorted(REQUIRED_TRIGGERS - triggers.keys())
        if missing_triggers:
            errors.append(f"missing immutable triggers: {missing_triggers}")
        for name in REQUIRED_TRIGGERS & triggers.keys():
            sql = triggers[name].upper()
            if "RAISE(ABORT" not in sql:
                errors.append(f"trigger {name} does not abort writes")
        evidence["immutableTriggers"] = sorted(triggers)
        operation_indexes = {
            row["name"]: row["sql"] or ""
            for row in connection.execute(
                "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_review_events_operation_id'"
            )
        }
        operation_index_sql = operation_indexes.get("idx_review_events_operation_id", "").upper()
        if "UNIQUE INDEX" not in operation_index_sql or "WHERE OPERATION_ID IS NOT NULL" not in operation_index_sql:
            errors.append("missing unique partial review-operation id index")
        evidence["operationIdIndex"] = bool(operation_index_sql)

        ledger_event_index = next(
            (
                row
                for row in connection.execute("PRAGMA index_list('review_compensation_ledger')")
                if row["name"] == "idx_review_compensation_one_entry_per_event"
            ),
            None,
        )
        ledger_event_index_sql = ""
        ledger_event_index_columns: list[str] = []
        if ledger_event_index is not None:
            index_row = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?",
                (ledger_event_index["name"],),
            ).fetchone()
            ledger_event_index_sql = " ".join(str(index_row["sql"] or "").upper().split()) if index_row else ""
            ledger_event_index_columns = [
                str(row["name"])
                for row in connection.execute(
                    f"PRAGMA index_info('{ledger_event_index['name']}')"
                )
            ]
        valid_ledger_event_index = (
            ledger_event_index is not None
            and int(ledger_event_index["unique"]) == 1
            and int(ledger_event_index["partial"]) == 1
            and ledger_event_index_columns == ["review_event_id"]
            and "".join(ledger_event_index_sql.split())
            == (
                "CREATEUNIQUEINDEXIDX_REVIEW_COMPENSATION_ONE_ENTRY_PER_EVENT"
                "ONREVIEW_COMPENSATION_LEDGER(REVIEW_EVENT_ID)WHEREREVIEW_EVENT_IDISNOTNULL"
            )
        )
        if not valid_ledger_event_index:
            errors.append(
                "missing/malformed unique partial idx_review_compensation_one_entry_per_event"
            )
        evidence["eventLedgerUniqueIndex"] = valid_ledger_event_index

        reversal_index = next(
            (
                row
                for row in connection.execute("PRAGMA index_list('review_compensation_ledger')")
                if row["name"] == "idx_review_compensation_one_reversal_per_entry"
            ),
            None,
        )
        reversal_index_sql = ""
        reversal_index_columns: list[str] = []
        if reversal_index is not None:
            row = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?",
                (reversal_index["name"],),
            ).fetchone()
            reversal_index_sql = " ".join(str(row["sql"] or "").upper().split()) if row else ""
            reversal_index_columns = [
                str(item["name"])
                for item in connection.execute(
                    f"PRAGMA index_info('{reversal_index['name']}')"
                )
            ]
        valid_reversal_index = (
            reversal_index is not None
            and int(reversal_index["unique"]) == 1
            and int(reversal_index["partial"]) == 1
            and reversal_index_columns == ["reverses_entry_id"]
            and "".join(reversal_index_sql.split())
            == (
                "CREATEUNIQUEINDEXIDX_REVIEW_COMPENSATION_ONE_REVERSAL_PER_ENTRY"
                "ONREVIEW_COMPENSATION_LEDGER(REVERSES_ENTRY_ID)"
                "WHEREREVERSES_ENTRY_IDISNOTNULL"
            )
        )
        if not valid_reversal_index:
            errors.append(
                "missing/malformed unique partial idx_review_compensation_one_reversal_per_entry"
            )
        evidence["ledgerReversalUniqueIndex"] = valid_reversal_index

        events = {
            int(row["id"]): row
            for row in connection.execute(
                """SELECT id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                          operation_id, operation_payload_hash, app_git_sha, playback_guard_version
                     FROM review_events
                    WHERE id > ? AND source IN ('couch', 'couch_spot_check')
                    ORDER BY id""",
                (cutoff,),
            )
        }
        all_ledger_rows = list(
            connection.execute(
                """SELECT id, entry_id, policy_version, review_event_id, canonical_work_id,
                          canonical_identity_kind, reviewer, segment_id, compensation_action,
                          effective_decision, duration_ms, rate_basis_points,
                          entitlement_micro_iqd, delta_micro_iqd,
                          corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id
                     FROM review_compensation_ledger ORDER BY id"""
            )
        )
        ledger_rows = [row for row in all_ledger_rows if row["policy_version"] == POLICY_VERSION]
        effective_event_ids = {event.event_id for event in hidden_state.effective_events}
        effective_ledger_ids = {event.ledger_id for event in hidden_state.effective_events}
        accounting_effective_events = int(
            connection.execute(
                """SELECT COUNT(*) FROM effective_review_events_v60
                    WHERE review_event_id > ? AND policy_version = ?
                      AND source IN ('couch', 'couch_spot_check')""",
                (cutoff, POLICY_VERSION),
            ).fetchone()[0]
        )
        evidence["postCutoffEvents"] = len(effective_event_ids)
        evidence["accountingEffectiveEvents"] = accounting_effective_events
        evidence["rawPostCutoffEvents"] = len(events)
        evidence["ledgerEntries"] = len(effective_ledger_ids)
        evidence["rawLedgerEntries"] = len(ledger_rows)
        evidence["reversalEntries"] = hidden_state.reversal_count
        evidence["allPolicyLedgerEntries"] = len(all_ledger_rows)

        event_entry_counts: dict[int, int] = {}
        all_policy_event_entry_counts: dict[int, int] = {}
        entry_by_id: dict[str, sqlite3.Row] = {}
        balances: dict[str, int] = {}
        corrected_balances: dict[str, int] = {}
        fallback_entries = 0
        for row in all_ledger_rows:
            review_event_id = row["review_event_id"]
            if review_event_id is not None:
                event_id = int(review_event_id)
                all_policy_event_entry_counts[event_id] = all_policy_event_entry_counts.get(event_id, 0) + 1
                if event_id in events and row["policy_version"] != POLICY_VERSION:
                    errors.append(
                        f"post-cutoff event {event_id} has a foreign-policy ledger consequence "
                        f"{row['policy_version']!r}"
                    )

        for row in ledger_rows:
            entry_id = str(row["entry_id"])
            entry_by_id[entry_id] = row
            work_id = str(row["canonical_work_id"])
            prior = balances.get(work_id, 0)
            prior_corrected = corrected_balances.get(work_id, 0)
            action = str(row["compensation_action"])
            delta = int(row["delta_micro_iqd"])
            delta_corrected = int(row["delta_corrected_ms"])
            if row["canonical_identity_kind"] == "segment_id_fallback":
                fallback_entries += 1

            review_event_id = row["review_event_id"]
            if review_event_id is not None:
                review_event_id = int(review_event_id)
                event_entry_counts[review_event_id] = event_entry_counts.get(review_event_id, 0) + 1
                event = events.get(review_event_id)
                if event is None:
                    errors.append(f"ledger entry {entry_id} points outside the prospective event range")
                else:
                    expected_action = str(event["compensation_action"] or "")
                    if action != expected_action or row["effective_decision"] != event["action"]:
                        errors.append(f"ledger entry {entry_id} disagrees with review event {review_event_id}")
                    if row["segment_id"] != event["segment_id"] or str(row["reviewer"]).casefold() != str(event["reviewer"]).casefold():
                        errors.append(f"ledger identity {entry_id} disagrees with review event {review_event_id}")
                    if int(row["duration_ms"]) != int(event["duration_ms"]):
                        errors.append(f"ledger duration {entry_id} disagrees with review event {review_event_id}")
                    segment = connection.execute(
                        """SELECT id, audio_content_hash, alignment_json, duration_ms
                             FROM speech_segments WHERE id = ?""",
                        (event["segment_id"],),
                    ).fetchone()
                    identity_reason = "segment is missing"
                    canonical = False
                    if segment is not None:
                        canonical, identity_reason, _audio_work_id = _canonical_focus_status(segment)
                    expected_work_id: str | None = None
                    if segment is not None and canonical:
                        expected_work_id, reviewer_reason = canonical_reviewer_work_id(
                            event["reviewer"],
                            segment["audio_content_hash"],
                            segment["alignment_json"],
                        )
                        if expected_work_id is None:
                            identity_reason = reviewer_reason
                    if (
                        expected_work_id is None
                        or row["canonical_identity_kind"] != CANONICAL_IDENTITY_KIND
                        or row["canonical_work_id"] != expected_work_id
                    ):
                        errors.append(
                            f"ledger canonical identity {entry_id} disagrees with event segment: "
                            f"expected kind={CANONICAL_IDENTITY_KIND!r}, work={expected_work_id!r}; "
                            f"reason={identity_reason or 'none'}"
                        )
                expected_bps = BASIS_POINTS.get(action)
                if expected_bps is None:
                    errors.append(f"ledger entry {entry_id} has unsupported action {action!r}")
                else:
                    try:
                        entitlement = _exact_entitlement(int(row["duration_ms"]), expected_bps) if expected_bps else 0
                    except ValueError as error:
                        errors.append(f"ledger entry {entry_id}: {error}")
                        entitlement = -1
                    if int(row["rate_basis_points"]) != expected_bps or int(row["entitlement_micro_iqd"]) != entitlement:
                        errors.append(f"ledger rate/entitlement mismatch at {entry_id}")
                    expected_delta = 0 if action == "skip" else entitlement - prior
                    if delta != expected_delta:
                        errors.append(f"ledger delta mismatch at {entry_id}: {delta} != {expected_delta}")
                    target_corrected = (
                        int(row["duration_ms"])
                        if action == "edit"
                        else prior_corrected
                        if action == "skip"
                        else 0
                    )
                    expected_corrected_delta = target_corrected - prior_corrected
                    if int(row["corrected_entitlement_ms"]) != target_corrected:
                        errors.append(
                            f"corrected entitlement mismatch at {entry_id}: "
                            f"{row['corrected_entitlement_ms']} != {target_corrected}"
                        )
                    if delta_corrected != expected_corrected_delta:
                        errors.append(
                            f"corrected delta mismatch at {entry_id}: "
                            f"{delta_corrected} != {expected_corrected_delta}"
                        )
            elif action == "undo":
                reverses = row["reverses_entry_id"]
                expected_delta = 0
                expected_corrected_delta = 0
                if reverses is not None:
                    target = entry_by_id.get(str(reverses))
                    if target is None:
                        errors.append(f"undo {entry_id} references a missing/later entry {reverses}")
                    else:
                        expected_delta = -int(target["delta_micro_iqd"])
                        expected_corrected_delta = -int(target["delta_corrected_ms"])
                if delta != expected_delta:
                    errors.append(f"undo delta mismatch at {entry_id}: {delta} != {expected_delta}")
                if delta_corrected != expected_corrected_delta:
                    errors.append(
                        f"undo corrected delta mismatch at {entry_id}: "
                        f"{delta_corrected} != {expected_corrected_delta}"
                    )
                expected_corrected_entitlement = prior_corrected + expected_corrected_delta
                if int(row["corrected_entitlement_ms"]) != expected_corrected_entitlement:
                    errors.append(
                        f"undo corrected entitlement mismatch at {entry_id}: "
                        f"{row['corrected_entitlement_ms']} != {expected_corrected_entitlement}"
                    )
            else:
                errors.append(f"ledger entry {entry_id} has neither review event nor undo semantics")

            balances[work_id] = prior + delta
            corrected_balances[work_id] = prior_corrected + delta_corrected
            if balances[work_id] < 0:
                errors.append(f"canonical work {work_id} has a negative running entitlement")
            if corrected_balances[work_id] < 0:
                errors.append(f"canonical work {work_id} has a negative corrected-audio entitlement")

        for event_id, event in events.items():
            action = str(event["compensation_action"] or "")
            if str(event["segment_id"]) not in focus_id_set:
                errors.append(f"post-cutoff event {event_id} is outside the exact active review focus")
            if action not in BASIS_POINTS:
                errors.append(f"post-cutoff event {event_id} lacks a valid compensation_action")
            if event_entry_counts.get(event_id, 0) != 1:
                errors.append(f"post-cutoff event {event_id} has {event_entry_counts.get(event_id, 0)} ledger entries")
            if all_policy_event_entry_counts.get(event_id, 0) != 1:
                errors.append(
                    f"post-cutoff event {event_id} has {all_policy_event_entry_counts.get(event_id, 0)} "
                    "total ledger entries across all policies"
                )
            if str(event["source"] or "").startswith("couch"):
                operation_id = str(event["operation_id"] or "")
                operation_hash = str(event["operation_payload_hash"] or "")
                try:
                    canonical_operation_id = str(uuid.UUID(operation_id))
                except (ValueError, AttributeError):
                    canonical_operation_id = ""
                if not operation_id or canonical_operation_id != operation_id:
                    errors.append(f"post-cutoff couch event {event_id} lacks a canonical operation UUID")
                if len(operation_hash) != 64 or any(char not in "0123456789abcdef" for char in operation_hash):
                    errors.append(f"post-cutoff couch event {event_id} lacks a canonical payload hash")
        evidence["durableOperationReceipts"] = sum(
            1
            for event_id, event in events.items()
            if event_id in effective_event_ids and str(event["operation_id"] or "")
        )
        evidence["rawDurableOperationReceipts"] = sum(
            1 for event in events.values() if str(event["operation_id"] or "")
        )
        evidence["fallbackLedgerEntries"] = fallback_entries
        if fallback_entries:
            errors.append(f"ledger contains {fallback_entries} fallback-identity entries")

        evidence.update(_audit_settlements(connection, ledger_rows, errors))

        focus_rows: dict[str, sqlite3.Row] = {}
        for offset in range(0, len(focus_ids), 900):
            chunk = focus_ids[offset : offset + 900]
            placeholders = ",".join("?" for _ in chunk)
            for row in connection.execute(
                f"SELECT id, audio_content_hash, alignment_json, duration_ms FROM speech_segments WHERE id IN ({placeholders})",
                chunk,
            ):
                focus_rows[str(row["id"])] = row
        missing_focus = [segment_id for segment_id in focus_ids if segment_id not in focus_rows]
        noncanonical_focus: list[str] = []
        focus_work_ids: dict[str, str] = {}
        duplicate_work_ids: list[tuple[str, str, str]] = []
        for segment_id, row in focus_rows.items():
            canonical, reason, work_id = _canonical_focus_status(row)
            if not canonical:
                noncanonical_focus.append(f"{segment_id}: {reason}")
            elif work_id is not None:
                prior = focus_work_ids.get(work_id)
                if prior is not None:
                    duplicate_work_ids.append((prior, segment_id, work_id))
                else:
                    focus_work_ids[work_id] = segment_id
        evidence["focusRows"] = len(focus_rows)
        evidence["canonicalFocusRows"] = len(focus_rows) - len(noncanonical_focus)
        if missing_focus:
            errors.append(f"focus contains {len(missing_focus)} missing segment IDs")
        if noncanonical_focus:
            errors.append(
                f"focus contains {len(noncanonical_focus)} noncanonical pay identities; first={noncanonical_focus[:3]}"
            )
        evidence["uniqueCanonicalFocusWorkIds"] = len(focus_work_ids)
        if duplicate_work_ids:
            errors.append(
                f"focus contains {len(duplicate_work_ids)} duplicate canonical pay identities; first={duplicate_work_ids[:3]}"
            )

        ledger_fk = list(connection.execute("PRAGMA foreign_key_check(review_compensation_ledger)"))
        policy_fk = list(connection.execute("PRAGMA foreign_key_check(review_compensation_policies)"))
        settlement_fk = list(connection.execute("PRAGMA foreign_key_check(review_compensation_settlements)"))
        evidence["compensationForeignKeyViolations"] = len(ledger_fk) + len(policy_fk) + len(settlement_fk)
        if ledger_fk or policy_fk or settlement_fk:
            errors.append(
                f"compensation tables have {len(ledger_fk) + len(policy_fk) + len(settlement_fk)} foreign-key violations"
            )

        evidence["totalEarnedMicroIqd"] = sum(int(row["delta_micro_iqd"]) for row in ledger_rows)
        evidence["correctedAudioMs"] = sum(corrected_balances.values())
        evidence["activeWorkBalances"] = sum(1 for value in balances.values() if value)

    return {**evidence, "ok": not errors, "errors": errors}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--db", type=Path)
    parser.add_argument("--focus", type=Path)
    args = parser.parse_args(argv)
    # Resolve the Windows %APPDATA% default only when a path actually needs it, so explicit
    # --db/--focus invocations (the policy tests, cross-platform CI) never require APPDATA.
    data_dir = args.data_dir
    if data_dir is None and not (args.db and args.focus):
        data_dir = _default_data_dir()
    db_path = args.db or data_dir / "cortex-speech.db"
    focus_path = args.focus or data_dir / "voice_focus.json"
    result = audit_flexible_paid(db_path)
    if result is None:
        result = audit(db_path, focus_path)
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    # Repeated on stderr so the unpaid-work banner survives in the sweep log even when the JSON body
    # is scrolled past; a PASS whose report nobody reads is how invisible work stays invisible.
    for warning in result.get("warnings", []):
        print(warning, file=sys.stderr)
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
