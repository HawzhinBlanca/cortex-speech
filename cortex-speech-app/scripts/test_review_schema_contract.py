#!/usr/bin/env python3
"""Regression tests for the exact paid-review SQLite schema contract."""

from __future__ import annotations

import importlib.util
import sqlite3
import sys
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_review_schema_contract.py")
VERIFY_10 = SCRIPT.parents[2] / "scripts" / "verify_10.py"


def load_gate():
    spec = importlib.util.spec_from_file_location("review_schema_contract_gate", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


GATE = load_gate()


def immutable_effect_trigger():
    objects, _digest = GATE.load_contract_objects()
    return objects[("trigger", "review_effect_state_immutable_update")]


def test_contract_is_extracted_from_the_canonical_migration_source():
    objects, digest = GATE.load_contract_objects()
    required = {
        ("trigger", "review_compensation_settlement_validate_insert"),
        ("trigger", "review_events_v60_provenance_validate_insert"),
        ("trigger", "playback_receipts_v60_span_validate_insert"),
        ("trigger", "playback_receipts_v60_policy3_immutable_update"),
        ("trigger", "playback_receipts_v60_policy3_immutable_delete"),
        ("trigger", "human_decision_effect_events_validate_review_event_insert"),
        ("trigger", "correction_memory_contributions_effect_validate_insert"),
        ("trigger", "legacy_agent_examples_v60_immutable_update"),
        ("trigger", "legacy_corrections_v60_immutable_update"),
        ("table", "legacy_agent_examples_v60"),
        ("table", "legacy_corrections_v60"),
        ("table", "review_flag_effect_events"),
        ("view", "effective_review_events_v60"),
        ("view", "effective_review_flag_effects_v60"),
        ("view", "active_corrections_v60"),
        ("view", "effective_correction_memory_v60"),
        ("index", "idx_review_compensation_one_reversal_per_entry"),
        ("trigger", "segments_ai"),
        ("trigger", "segments_ad"),
        ("trigger", "segments_au"),
        ("trigger", "speech_segments_review_revision"),
        ("table", "independent_review_decisions"),
        ("view", "effective_independent_review_decisions_v61"),
        ("table", "review_pool_registry"),
        ("table", "review_pool_members"),
        ("table", "review_pool_decisions"),
        ("view", "effective_review_pool_decisions_v62"),
        ("table", "review_pool_owner_adjudications"),
        ("table", "review_pool_voice_certificates"),
        ("table", "review_pool_dedup_manifests"),
        ("table", "review_pool_duplicate_exclusions"),
        ("trigger", "speech_segments_v64_excluded_review_guard"),
    }
    assert len(objects) >= 120, f"contract unexpectedly shrank to {len(objects)} objects"
    assert required <= objects.keys(), sorted(required - objects.keys())
    assert len(digest) == 64 and set(digest) <= set("0123456789abcdef")


def test_contract_replays_the_schema_65_excluded_review_guard_replacement():
    objects, _digest = GATE.load_contract_objects()
    guard = objects[("trigger", "speech_segments_v64_excluded_review_guard")].sql
    assert "review_revision" not in guard
    for protected_column in (
        "human_decision",
        "verdict",
        "verdict_transcript",
        "annotated_transcript",
        "verified",
        "reviewed_by",
    ):
        assert protected_column in guard


def test_contract_preserves_raw_phone_action_and_strict_integer_source_spans():
    objects, _digest = GATE.load_contract_objects()
    provenance = objects[("trigger", "review_events_v60_provenance_validate_insert")].sql
    playback = objects[("trigger", "playback_receipts_v60_span_validate_insert")].sql

    assert "'bad'" in provenance, "raw phone reject request must remain rederivable"
    assert "json_type(s.alignment_json, '$.source_start_ms') = 'integer'" in playback
    assert "json_type(s.alignment_json, '$.source_end_ms') = 'integer'" in playback
    assert "abs( s.duration_ms - (new.source_end_ms - new.source_start_ms) ) <= 1" in playback


def test_whitespace_does_not_create_a_false_schema_mismatch():
    expected = immutable_effect_trigger()
    connection = sqlite3.connect(":memory:")
    connection.execute("CREATE TABLE review_effect_state(singleton_key INTEGER)")
    connection.executescript(expected.sql)
    errors, protected = GATE.compare_schema_objects(
        connection,
        {(expected.object_type, expected.name): expected},
    )
    connection.close()
    assert errors == []
    assert protected == {"review_effect_state"}


def test_same_name_dummy_abort_trigger_is_not_accepted_as_the_contract():
    expected = immutable_effect_trigger()
    connection = sqlite3.connect(":memory:")
    connection.execute("CREATE TABLE review_effect_state(singleton_key INTEGER)")
    connection.execute(
        "CREATE TRIGGER review_effect_state_immutable_update "
        "BEFORE UPDATE ON review_effect_state BEGIN SELECT RAISE(ABORT, 'dummy'); END"
    )
    errors, _protected = GATE.compare_schema_objects(
        connection,
        {(expected.object_type, expected.name): expected},
    )
    connection.close()
    assert errors == ["schema contract SQL mismatch for trigger:review_effect_state_immutable_update"]


def test_missing_contract_object_fails_closed():
    expected = immutable_effect_trigger()
    connection = sqlite3.connect(":memory:")
    errors, _protected = GATE.compare_schema_objects(
        connection,
        {(expected.object_type, expected.name): expected},
    )
    connection.close()
    assert errors == ["missing schema contract object trigger:review_effect_state_immutable_update"]


def test_unexpected_trigger_on_a_protected_table_fails_closed():
    expected = immutable_effect_trigger()
    connection = sqlite3.connect(":memory:")
    connection.execute("CREATE TABLE review_effect_state(singleton_key INTEGER)")
    connection.executescript(expected.sql)
    connection.execute(
        "CREATE TRIGGER extra_effect_mutation AFTER UPDATE ON review_effect_state BEGIN SELECT 1; END"
    )
    errors, _protected = GATE.compare_schema_objects(
        connection,
        {(expected.object_type, expected.name): expected},
    )
    connection.close()
    assert errors == ["unexpected trigger extra_effect_mutation on protected table review_effect_state"]


def test_alter_added_foreign_key_cannot_be_replaced_by_a_same_named_plain_column():
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        """
        CREATE TABLE human_decision_effect_events(id INTEGER PRIMARY KEY);
        CREATE TABLE review_events(
            compensation_action TEXT, operation_id TEXT, operation_payload_hash TEXT,
            app_git_sha TEXT, playback_guard_version TEXT,
            requested_action TEXT, requested_transcript TEXT
        );
        CREATE TABLE agent_examples(effect_event_id INTEGER);
        CREATE TABLE corrections(
            effect_event_id INTEGER REFERENCES human_decision_effect_events(id)
        );
        CREATE TABLE correction_memory(legacy_seed INTEGER NOT NULL DEFAULT 1);
        CREATE TABLE playback_receipts(source_start_ms INTEGER, source_end_ms INTEGER);
        """
    )
    errors = GATE.audit_added_columns(connection)
    connection.close()
    assert errors == [
        "missing schema contract foreign key agent_examples.effect_event_id.human_decision_effect_events.id"
    ]


def test_master_release_verdict_runs_the_live_schema_contract_unskippably():
    spec = importlib.util.spec_from_file_location("verify_10_review_schema_contract", VERIFY_10)
    assert spec is not None and spec.loader is not None
    verify = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = verify
    spec.loader.exec_module(verify)
    matches = [entry for entry in verify.GATES if entry[0] == "review-schema-contract-live"]
    assert len(matches) == 1
    _name, tier, kind, payload, cwd, probe, _charter = matches[0]
    assert (tier, kind, cwd) == (2, "cmd", verify.APP)
    assert probe is None, "the live schema contract must never be skipped"
    assert str(SCRIPT) in payload


if __name__ == "__main__":
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"review schema contract regressions passed ({len(tests)} assertions)")
