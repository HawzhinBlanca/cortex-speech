#!/usr/bin/env python3
"""Fail-closed production gate for Cortex reviewer compensation.

This gate reads the migrated live database and active voice focus without modifying either.  It
proves the authorized policy constants, one ledger consequence per effective post-cutoff event,
append-only triggers, durable schema-v60 hidden-key grants, the 24-action pilot ceiling, signed
re-decision/reversal arithmetic, and canonical audio identity for every focused clip.  A pre-v60
database cannot be called ready merely because the source tree contains the new release code.
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

from pilot_focus_contract import (
    CANONICAL_IDENTITY_KIND,
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


def audit(db_path: Path, focus_path: Path) -> dict[str, Any]:
    errors: list[str] = []
    evidence: dict[str, Any] = {
        "database": str(db_path.resolve()),
        "focus": str(focus_path.resolve()),
        "policyVersion": POLICY_VERSION,
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
        evidence["settlements"] = len(settlements)
        evidence["settledMicroIqd"] = sum(int(row["allocated_micro_iqd"]) for row in settlements)

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
    data_dir = args.data_dir or _default_data_dir()
    db_path = args.db or data_dir / "cortex-speech.db"
    focus_path = args.focus or data_dir / "voice_focus.json"
    result = audit(db_path, focus_path)
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
