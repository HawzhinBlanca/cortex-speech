#!/usr/bin/env python3
"""Temp-only regressions for the fail-closed recovery drill."""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import sys
import tempfile
from pathlib import Path
from typing import Any

from pilot_focus_contract import contract_for_ids, verify_controlled_pilot_focus

SCRIPT = Path(__file__).with_name("restore_drill.py")
SPEC = importlib.util.spec_from_file_location("restore_drill", SCRIPT)
assert SPEC and SPEC.loader
drill_module = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = drill_module
SPEC.loader.exec_module(drill_module)
TEST_FOCUS_CONTRACT = contract_for_ids(["segment-1"])
drill_module.verify_controlled_pilot_focus = lambda root: verify_controlled_pilot_focus(root, TEST_FOCUS_CONTRACT)

DEPLOYMENT_SHA = "a" * 64


def seed_tree(
    root: Path, *, policy: bool = True, absent_state: tuple[str, ...] = ()
) -> None:
    connection = sqlite3.connect(root / drill_module.DB_FILE)
    connection.executescript(
        f"""
        PRAGMA foreign_keys=ON;
        CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);
        CREATE TABLE speech_segments(id TEXT PRIMARY KEY);
        INSERT INTO speech_segments VALUES('segment-1');
        CREATE TABLE review_events(id INTEGER PRIMARY KEY);
        INSERT INTO review_events VALUES(2);
        INSERT INTO review_events VALUES(5);
        CREATE TABLE spot_checks(id INTEGER PRIMARY KEY);
        CREATE TABLE model_versions(
            id TEXT PRIMARY KEY,
            checkpoint_sha256 TEXT NOT NULL,
            status TEXT NOT NULL
        );
        INSERT INTO model_versions VALUES('champion-v1', '{DEPLOYMENT_SHA}', 'champion');
        CREATE TABLE import_jobs(id TEXT PRIMARY KEY);
        CREATE TABLE import_job_files(id INTEGER PRIMARY KEY);
        """
    )
    connection.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        drill_module.source_migrations(drill_module.DEFAULT_MIGRATIONS),
    )
    connection.commit()
    connection.close()
    (root / "settings.json").write_text("{}", encoding="utf-8")
    (root / "champion.json").write_text(
        json.dumps(
            {
                "champions": {
                    "omniasr-7b": {
                        "modelVersionId": "champion-v1",
                        "deploymentSha256": DEPLOYMENT_SHA,
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    (root / "reviewer_dialects.json").write_text("{}", encoding="utf-8")
    (root / "voice_focus.json").write_text(
        '{"name":"test","segment_ids":["segment-1"]}', encoding="utf-8"
    )
    for name in absent_state:
        (root / name).unlink()
        (root / drill_module.state_absence_marker(name)).write_bytes(
            drill_module.state_absence_bytes(name)
        )
    if policy:
        write_policy(root, baseline=5)
    else:
        (root / drill_module.REVIEW_PILOT_ABSENT_FILE).write_bytes(
            drill_module.REVIEW_PILOT_ABSENT_BYTES
        )


def write_policy(root: Path, *, baseline: int) -> None:
    (root / drill_module.REVIEW_PILOT_FILE).write_text(
        json.dumps(
            {
                "schema_version": 1,
                "after_review_event_id": baseline,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    {"name": "Hawzhin", "max_corpus_actions": 10},
                    {"name": "Pavel", "max_corpus_actions": 10},
                ],
            }
        ),
        encoding="utf-8",
    )


def inventory(root: Path) -> list[dict[str, Any]]:
    return [
        {
            "path": path.name,
            "sizeBytes": path.stat().st_size,
            "sha256": drill_module.sha256_of(path),
        }
        for path in sorted(root.iterdir(), key=lambda item: item.name)
        if path.is_file() and path.name != drill_module.MANIFEST
    ]


def write_manifest(root: Path, schema: int) -> dict[str, Any]:
    files = inventory(root)
    if schema == 1:
        payload: dict[str, Any] = {
            "schema": 1,
            "reviewPilotPolicyStateSchema": 1,
            "createdAtEpochSecs": 1,
            "appGitSha": "test",
            "files": files,
        }
    else:
        payload = {
            "schema": 2,
            "createdAtEpochSecs": 1,
            "appGitSha": "test",
            "sourceDataDir": "C:/isolated-test-profile",
            "databaseEvidence": drill_module.inspect_database(root / drill_module.DB_FILE).evidence,
            "files": files,
        }
    write_payload(root, payload)
    return payload


def write_payload(root: Path, payload: dict[str, Any]) -> None:
    (root / drill_module.MANIFEST).write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )


def fresh_tree(
    root: Path,
    schema: int = 1,
    *,
    policy: bool = True,
    absent_state: tuple[str, ...] = (),
) -> dict[str, Any]:
    seed_tree(root, policy=policy, absent_state=absent_state)
    return write_manifest(root, schema)


def assert_refused(root: Path, expected: str) -> None:
    problems = drill_module.drill(root)
    assert problems, "the malformed snapshot unexpectedly passed"
    assert expected in "\n".join(problems), problems


def remove_file_and_row(root: Path, payload: dict[str, Any], name: str) -> None:
    (root / name).unlink()
    payload["files"] = [row for row in payload["files"] if row["path"] != name]
    write_payload(root, payload)


def test_both_manifest_schemas_accept_policy_and_exact_absence() -> None:
    for schema in (1, 2):
        for policy in (True, False):
            for absent_state in ((), drill_module.REQUIRED_STATE):
                with tempfile.TemporaryDirectory() as raw:
                    root = Path(raw)
                    fresh_tree(root, schema, policy=policy, absent_state=absent_state)
                    if policy and "voice_focus.json" in absent_state:
                        assert_refused(root, "voice_focus.json is required")
                    else:
                        assert drill_module.drill(root) == []


def test_policy_bearing_manifest_rejects_self_consistent_wrong_focus() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fresh_tree(root, schema=2)
        (root / "voice_focus.json").write_text(
            json.dumps({"segment_ids": ["segment-wrong"]}), encoding="utf-8"
        )
        # Rebuild the manifest so every size/hash is honest. The semantic contract must still refuse.
        write_manifest(root, schema=2)
        assert_refused(root, "digest mismatch")


def test_production_drill_refuses_manifestless_legacy_tree() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        assert_refused(root, drill_module.MANIFEST)


def test_manifest_rejects_duplicate_keys_and_non_object_or_extra_root_shape() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fresh_tree(root)
        manifest = root / drill_module.MANIFEST
        text = manifest.read_text(encoding="utf-8")
        manifest.write_text(text.replace('"schema": 1', '"schema": 1, "schema": 1', 1), encoding="utf-8")
        assert_refused(root, "duplicate JSON object key")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["unexpected"] = True
        write_payload(root, payload)
        assert_refused(root, "extra=['unexpected']")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fresh_tree(root)
        (root / drill_module.MANIFEST).write_text("[]", encoding="utf-8")
        assert_refused(root, "must be a JSON object")


def test_manifest_rejects_strict_file_row_shape_type_and_duplicate() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["files"][0]["extra"] = 1
        write_payload(root, payload)
        assert_refused(root, "manifest file row 0 fields are invalid")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["files"][0]["sizeBytes"] = True
        write_payload(root, payload)
        assert_refused(root, "must be a non-negative integer")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["files"].append(dict(payload["files"][0]))
        write_payload(root, payload)
        assert_refused(root, "duplicate file")


def test_manifest_rejects_unsafe_nonportable_paths() -> None:
    for unsafe in ("../escape", "nested/file", "nested\\file", "C:\\escape", "CON", drill_module.MANIFEST):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            payload = fresh_tree(root)
            payload["files"][0]["path"] = unsafe
            write_payload(root, payload)
            assert_refused(root, "unsafe file path")


def test_manifest_inventory_is_exact_and_hash_and_size_bound() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fresh_tree(root)
        (root / "unlisted.txt").write_text("not in manifest", encoding="utf-8")
        assert_refused(root, "unlisted=['unlisted.txt']")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["files"][0]["sizeBytes"] += 1
        write_payload(root, payload)
        assert_refused(root, "size mismatch")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        payload["files"][0]["sha256"] = "0" * 64
        write_payload(root, payload)
        assert_refused(root, "SHA-256 mismatch")


def test_database_and_one_exact_representation_per_state_are_mandatory() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        remove_file_and_row(root, payload, drill_module.DB_FILE)
        assert_refused(root, f"'{drill_module.DB_FILE}'")

    for name in drill_module.REQUIRED_STATE:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            payload = fresh_tree(root)
            remove_file_and_row(root, payload, name)
            assert_refused(root, f"exactly one of {name}")

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fresh_tree(root)
            marker = root / drill_module.state_absence_marker(name)
            marker.write_bytes(drill_module.state_absence_bytes(name))
            write_manifest(root, 1)
            assert_refused(root, f"exactly one of {name}")

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fresh_tree(root, absent_state=(name,))
            marker = root / drill_module.state_absence_marker(name)
            marker.write_bytes(b"wrong\n")
            write_manifest(root, 1)
            assert_refused(root, "invalid contents")


def test_pilot_state_is_exactly_one_valid_policy_or_exact_absence_marker() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        (root / drill_module.REVIEW_PILOT_ABSENT_FILE).write_bytes(
            drill_module.REVIEW_PILOT_ABSENT_BYTES
        )
        write_manifest(root, 1)
        assert_refused(root, "exactly one")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root)
        remove_file_and_row(root, payload, drill_module.REVIEW_PILOT_FILE)
        assert_refused(root, "exactly one")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fresh_tree(root, policy=False)
        (root / drill_module.REVIEW_PILOT_ABSENT_FILE).write_bytes(b"wrong\n")
        write_manifest(root, 1)
        assert_refused(root, "invalid contents")


def test_pilot_policy_rejects_duplicate_or_weakened_shape_and_ahead_baseline() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        policy = root / drill_module.REVIEW_PILOT_FILE
        text = policy.read_text(encoding="utf-8")
        policy.write_text(
            text.replace('"schema_version": 1', '"schema_version": 1, "schema_version": 1'),
            encoding="utf-8",
        )
        write_manifest(root, 1)
        assert_refused(root, "duplicate JSON object key")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        value = json.loads((root / drill_module.REVIEW_PILOT_FILE).read_text(encoding="utf-8"))
        value["reviewers"][0]["max_corpus_actions"] = 11
        (root / drill_module.REVIEW_PILOT_FILE).write_text(json.dumps(value), encoding="utf-8")
        write_manifest(root, 1)
        assert_refused(root, "exactly 10")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        write_policy(root, baseline=6)
        write_manifest(root, 1)
        assert_refused(root, "6 > 5")


def test_schema2_evidence_shape_and_values_are_bound_to_restored_database() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root, 2)
        payload["databaseEvidence"]["extra"] = 1
        write_payload(root, payload)
        assert_refused(root, "databaseEvidence fields are invalid")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root, 2)
        payload["databaseEvidence"]["schemaVersion"] = 56
        write_payload(root, payload)
        assert_refused(root, "does not exactly match")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        payload = fresh_tree(root, 2)
        del payload["databaseEvidence"]["rowCounts"]["import_jobs"]
        write_payload(root, payload)
        assert_refused(root, "rowCounts fields are invalid")


def test_required_json_state_rejects_duplicate_keys_after_manifest_verifies() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        (root / "settings.json").write_text('{"theme":"Dark","theme":"Light"}', encoding="utf-8")
        write_manifest(root, 1)
        assert_refused(root, "duplicate JSON object key")


def test_migration_history_requires_an_exact_description_bound_canonical_prefix() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        seed_tree(root)
        connection = sqlite3.connect(root / drill_module.DB_FILE)
        connection.execute("DELETE FROM schema_migrations WHERE version > 57")
        connection.commit()
        connection.close()
        write_manifest(root, 1)
        assert drill_module.drill(root) == [], "an exact older canonical prefix is restore-valid"

    mutations = (
        ("DELETE FROM schema_migrations WHERE version <> 57", "missing="),
        ("DELETE FROM schema_migrations WHERE version = 23", "missing=[23]"),
        (
            "UPDATE schema_migrations SET description = 'tampered' WHERE version = 31",
            "descriptionMismatch=[31]",
        ),
        ("DROP TABLE schema_migrations", "missing or empty"),
        (
            "INSERT INTO schema_migrations(version, description) VALUES(99999, 'future')",
            "newer than this source supports",
        ),
    )
    for statement, expected in mutations:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            seed_tree(root)
            connection = sqlite3.connect(root / drill_module.DB_FILE)
            connection.execute(statement)
            connection.commit()
            connection.close()
            write_manifest(root, 1)
            assert_refused(root, expected)


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"RESTORE DRILL: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
