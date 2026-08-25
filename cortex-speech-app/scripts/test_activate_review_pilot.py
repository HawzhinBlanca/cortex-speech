#!/usr/bin/env python3
"""Regression tests for fail-closed controlled-pilot activation."""

from __future__ import annotations

import json
import sqlite3
import tempfile
from unittest import mock
from pathlib import Path

import activate_review_pilot as activator
from activate_review_pilot import (
    POLICY_FILE,
    REVOCATION_FILE,
    SESSION_FILE,
    acquire_cortex_lock,
    pilot_policy,
    prepare_maintenance_revocation,
    sha256_file,
    validate_pilot_policy,
)
from check_database_integrity import DEFAULT_MIGRATIONS, source_migrations
from pilot_focus_contract import (
    PilotFocusError,
    contract_for_ids,
    load_pilot_focus_contract,
    verify_controlled_pilot_focus,
)
from review_pilot_hidden_contract import HIDDEN_SCHEMA_SQL, policy_sha256, parse_policy

TEST_FOCUS_IDS = ("focus-a", "focus-b", "focus-c")
TEST_FOCUS_CONTRACT = contract_for_ids(TEST_FOCUS_IDS)


def _verify_test_focus(data_dir: Path):
    return verify_controlled_pilot_focus(data_dir, TEST_FOCUS_CONTRACT)


def activate(*args, **kwargs):
    """Exercise the real activator with an explicit small test contract; CLI has no override."""
    with (
        mock.patch.object(activator, "verify_controlled_pilot_focus", side_effect=_verify_test_focus),
        mock.patch.object(activator, "load_pilot_focus_contract", return_value=TEST_FOCUS_CONTRACT),
    ):
        return activator.activate(*args, **kwargs)


def seed(root: Path, *, schema: int = 66) -> tuple[Path, dict[str, object]]:
    db_path = root / "cortex-speech.db"
    conn = sqlite3.connect(db_path)
    conn.executescript(
        f"""
        PRAGMA foreign_keys=ON;
        CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);
        CREATE TABLE review_events(
            id INTEGER PRIMARY KEY, segment_id TEXT NOT NULL, reviewer TEXT NOT NULL,
            action TEXT NOT NULL, source TEXT NOT NULL
        );
        INSERT INTO review_events VALUES(863, 'legacy-segment', 'legacy', 'accept', 'couch');
        CREATE TABLE review_compensation_policies(policy_version TEXT PRIMARY KEY);
        INSERT INTO review_compensation_policies VALUES('review-iqd-v1-2026-08-21');
        """
    )
    if schema >= 59:
        conn.executescript(HIDDEN_SCHEMA_SQL)
    if schema >= 60:
        conn.executescript(
            """
            CREATE TABLE review_compensation_ledger(id INTEGER PRIMARY KEY);
            CREATE TABLE human_decision_effect_events(id INTEGER PRIMARY KEY);
            CREATE TABLE human_decision_effect_reversals(id INTEGER PRIMARY KEY);
            CREATE TABLE review_flag_effect_events(id INTEGER PRIMARY KEY);
            CREATE TABLE review_flag_effect_reversals(id INTEGER PRIMARY KEY);
            """
        )
    conn.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        [entry for entry in source_migrations(DEFAULT_MIGRATIONS) if entry[0] <= schema],
    )
    conn.commit()
    conn.close()
    session: dict[str, object] = {
        "reviewers": {
            "dpapi-rubar": "Rubar",
            "dpapi-alle": "Alle",
            "dpapi-sewa": "Sewa",
        },
        "db_path": str(db_path),
        "spot_checks": [["r1", "Rubar"], ["a1", "Alle"], ["s1", "Sewa"]],
        "pilot_spot_checks": [],
        "sessions": [
            {"token": "cookie-r", "reviewer": "Rubar", "issued_unix": 1},
            {"token": "cookie-a", "reviewer": "Alle", "issued_unix": 2},
            {"token": "cookie-s", "reviewer": "Sewa", "issued_unix": 3},
        ],
    }
    (root / SESSION_FILE).write_text(json.dumps(session), encoding="utf-8")
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "test", "segment_ids": list(TEST_FOCUS_IDS)}),
        encoding="utf-8",
    )
    return db_path, session


def test_activation_preserves_target_tokens_narrows_every_session_surface_and_backs_up() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, original = seed(root)
        result = activate(
            root,
            db_path,
            expected_max_review_event_id=863,
            check_runtime=False,
        )
        policy = json.loads((root / POLICY_FILE).read_text(encoding="utf-8"))
        session = json.loads((root / SESSION_FILE).read_text(encoding="utf-8"))
        assert policy == session["pilot_policy"]
        assert [entry["name"] for entry in policy["reviewers"]] == ["Alle", "Rubar"]
        assert policy["after_review_event_id"] == 863
        assert policy["max_total_corpus_actions"] == 20
        assert result["maxCorpusActions"] == 20
        assert result["maxHiddenQcActions"] == 4
        assert result["maxCompensatedUiActions"] == 24
        assert result["controlledPilotFocusCount"] == len(TEST_FOCUS_IDS)
        assert result["controlledPilotFocusDigest"] == TEST_FOCUS_CONTRACT.sorted_unique_segment_ids_sha256
        assert session["reviewers"] == {
            "dpapi-rubar": "Rubar",
            "dpapi-alle": "Alle",
        }, "protected DPAPI token bytes must be preserved exactly"
        assert {entry["reviewer"] for entry in session["sessions"]} == {"Rubar", "Alle"}
        assert {entry[1] for entry in session["spot_checks"]} == {"Rubar", "Alle"}
        assert session["pilot_spot_checks"] == []
        assert not (root / REVOCATION_FILE).exists()
        backup = Path(result["backup"])
        assert json.loads((backup / SESSION_FILE).read_text(encoding="utf-8")) == original
        assert (backup / f"{POLICY_FILE}.ABSENT").is_file()


def test_first_activation_refuses_unnamespaced_legacy_pilot_hidden_keys() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, _ = seed(root)
        session_path = root / SESSION_FILE
        session = json.loads(session_path.read_text(encoding="utf-8"))
        session["pilot_spot_checks"] = [["ambiguous-key", "Rubar"]]
        session_path.write_text(json.dumps(session), encoding="utf-8")

        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except RuntimeError as error:
            assert "without the policy" in str(error), error
        else:
            raise AssertionError("unnamespaced legacy hidden keys were reinterpreted as a new pilot")
        assert not (root / POLICY_FILE).exists()


def test_event_id_and_existing_policy_are_compare_and_swap_guards() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, _ = seed(root)
        try:
            activate(root, db_path, expected_max_review_event_id=862, check_runtime=False)
        except RuntimeError as error:
            assert "CAS mismatch" in str(error)
        else:
            raise AssertionError("stale event-id precondition was accepted")
        assert not (root / POLICY_FILE).exists()

        existing = root / POLICY_FILE
        existing.write_text("{}", encoding="utf-8")
        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except RuntimeError as error:
            assert "expected-policy-sha256" in str(error)
        else:
            raise AssertionError("an existing policy was overwritten without a hash CAS")
        assert sha256_file(existing) == sha256_file(existing)


def test_pristine_roster_replacement_atomically_extends_the_exact_focus() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, credential_session = seed(root)
        credential_path = root / "credential_session.json"
        credential_path.write_text(json.dumps(credential_session), encoding="utf-8")

        old_policy = pilot_policy(863)
        old_policy["reviewers"] = [
            {"name": "Hawzhin", "max_corpus_actions": 10},
            {"name": "Pavel", "max_corpus_actions": 10},
        ]
        policy_path = root / POLICY_FILE
        policy_path.write_text(json.dumps(old_policy), encoding="utf-8")
        current_session = {
            "reviewers": {"old-h": "Hawzhin", "old-p": "Pavel"},
            "db_path": str(db_path),
            "spot_checks": [],
            "pilot_spot_checks": [],
            "sessions": [],
            "pilot_policy": old_policy,
        }
        (root / SESSION_FILE).write_text(json.dumps(current_session), encoding="utf-8")
        focus_path = root / "voice_focus.json"
        focus_path.write_text(
            json.dumps({"name": "test", "segment_ids": list(TEST_FOCUS_IDS[:2])}),
            encoding="utf-8",
        )
        old_focus_hash = sha256_file(focus_path)

        result = activate(
            root,
            db_path,
            expected_max_review_event_id=863,
            expected_policy_sha256=sha256_file(policy_path),
            credential_session=credential_path,
            replace_roster_before_activity=True,
            focus_additions=(TEST_FOCUS_IDS[2],),
            expected_focus_sha256=old_focus_hash,
            check_runtime=False,
        )

        assert result["rosterReplacedBeforeActivity"] is True
        assert set(json.loads((root / SESSION_FILE).read_text())["reviewers"].values()) == {
            "Rubar",
            "Alle",
        }
        assert set(json.loads(focus_path.read_text())["segment_ids"]) == set(TEST_FOCUS_IDS)
        backup = Path(result["backup"])
        assert sha256_file(backup / "voice_focus.json") == old_focus_hash
        assert not (root / REVOCATION_FILE).exists()


def test_existing_pilot_cannot_reset_its_baseline_after_any_durable_activity() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, _ = seed(root)
        activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        policy_path = root / POLICY_FILE
        policy_hash = sha256_file(policy_path)
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO review_events VALUES(864, 'corpus-work', 'Rubar', 'accept', 'couch')"
        )
        conn.commit()
        conn.close()

        result = activate(
            root,
            db_path,
            expected_max_review_event_id=864,
            expected_policy_sha256=policy_hash,
            check_runtime=False,
        )
        assert result["afterReviewEventId"] == 863
        assert result["activationMaxReviewEventId"] == 864
        assert json.loads(policy_path.read_text(encoding="utf-8"))["after_review_event_id"] == 863


def test_schema63_activation_imports_session_and_completed_hidden_keys_into_one_namespace() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, _ = seed(root)
        activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        policy_path = root / POLICY_FILE
        policy_hash = sha256_file(policy_path)
        policy = json.loads(policy_path.read_text(encoding="utf-8"))
        session_path = root / SESSION_FILE
        session = json.loads(session_path.read_text(encoding="utf-8"))
        session["pilot_spot_checks"] = [
            ["hidden-r-session", "Rubar"],
            ["hidden-a-session", "Alle"],
        ]
        session_path.write_text(json.dumps(session), encoding="utf-8")
        conn = sqlite3.connect(db_path)
        conn.executemany(
            "INSERT INTO review_events VALUES(?, ?, ?, ?, 'couch_spot_check')",
            [
                (864, "hidden-r-completed", "Rubar", "accept"),
                (865, "hidden-a-completed", "Alle", "reject"),
            ],
        )
        conn.commit()
        conn.close()

        result = activate(
            root,
            db_path,
            expected_max_review_event_id=865,
            expected_policy_sha256=policy_hash,
            check_runtime=False,
        )
        expected_digest = policy_sha256(parse_policy(policy))
        conn = sqlite3.connect(db_path)
        rows = conn.execute(
            """SELECT policy_sha256, after_review_event_id, reviewer, segment_id
                 FROM review_pilot_hidden_keys ORDER BY reviewer, segment_id"""
        ).fetchall()
        conn.close()
        assert rows == [
            (expected_digest, 863, "Alle", "hidden-a-completed"),
            (expected_digest, 863, "Alle", "hidden-a-session"),
            (expected_digest, 863, "Rubar", "hidden-r-completed"),
            (expected_digest, 863, "Rubar", "hidden-r-session"),
        ]
        assert result["policySemanticSha256"] == expected_digest
        assert result["hiddenKeysImported"] == 4
        assert result["hiddenKeysDurable"] == 4
        assert result["afterReviewEventId"] == 863


def test_schema63_activation_rolls_back_when_hidden_history_exceeds_quota_or_schema_is_inexact() -> None:
    for mutation, expected in (
        ("over_quota", "lifetime set exceeds"),
        ("missing_trigger", "trigger(s) missing"),
    ):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            db_path, _ = seed(root)
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
            policy_path = root / POLICY_FILE
            policy_hash = sha256_file(policy_path)
            session_path = root / SESSION_FILE
            session_before = session_path.read_bytes()
            conn = sqlite3.connect(db_path)
            if mutation == "over_quota":
                conn.executemany(
                    "INSERT INTO review_events VALUES(?, ?, 'Rubar', 'accept', 'couch_spot_check')",
                    [(864, "hidden-one"), (865, "hidden-two"), (866, "hidden-three")],
                )
                expected_max = 866
            else:
                conn.execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert")
                expected_max = 863
            conn.commit()
            conn.close()
            try:
                activate(
                    root,
                    db_path,
                    expected_max_review_event_id=expected_max,
                    expected_policy_sha256=policy_hash,
                    check_runtime=False,
                )
            except RuntimeError as error:
                assert expected in str(error), error
            else:
                raise AssertionError(f"invalid hidden authority escaped: {mutation}")
            conn = sqlite3.connect(db_path)
            assert conn.execute("SELECT COUNT(*) FROM review_pilot_hidden_keys").fetchone()[0] == 0
            conn.close()
            assert policy_path.is_file()
            assert session_path.read_bytes() == session_before


def test_interrupted_activation_leaves_durable_revocation_instead_of_old_unrestricted_resume() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, original = seed(root)
        try:
            activate(
                root,
                db_path,
                expected_max_review_event_id=863,
                check_runtime=False,
                fail_after_revocation_for_test=True,
            )
        except RuntimeError as error:
            assert "injected" in str(error)
        else:
            raise AssertionError("injected interruption did not abort")
        assert (root / REVOCATION_FILE).is_file(), "a restart must remain denied after interruption"
        assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_schema_56_is_refused_before_any_activation_file_changes() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, original = seed(root, schema=56)
        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except RuntimeError as error:
            assert "schema 56/66" in str(error)
        else:
            raise AssertionError("pre-compensation database was accepted")
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()
        assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_missing_middle_or_description_drift_is_refused_before_file_changes() -> None:
    for sql, expected in (
        ("DELETE FROM schema_migrations WHERE version=23", "missing=[23]"),
        ("UPDATE schema_migrations SET description='tampered' WHERE version=31", "descriptionMismatch=[31]"),
    ):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            db_path, original = seed(root)
            conn = sqlite3.connect(db_path)
            conn.execute(sql)
            conn.commit()
            conn.close()
            try:
                activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
            except RuntimeError as error:
                assert expected in str(error), error
            else:
                raise AssertionError("damaged migration history was accepted")
            assert not (root / POLICY_FILE).exists()
            assert not (root / REVOCATION_FILE).exists()
            assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_non_restartable_or_duplicate_session_json_is_refused_before_revocation() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, session = seed(root)
        session["sessions"][0]["issued_unix"] = True
        session_path = root / SESSION_FILE
        session_path.write_text(json.dumps(session), encoding="utf-8")
        before = session_path.read_bytes()
        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except RuntimeError as error:
            assert "cannot survive restart" in str(error)
        else:
            raise AssertionError("a non-serde cookie session was accepted")
        assert session_path.read_bytes() == before
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, session = seed(root)
        session_path = root / SESSION_FILE
        encoded_db = json.dumps(str(db_path))
        session_path.write_text(
            '{"reviewers":{"dpapi-secret":"Rubar","dpapi-secret":"Alle"},'
            f'"db_path":{encoded_db}}}',
            encoding="utf-8",
        )
        before = session_path.read_bytes()
        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except ValueError as error:
            assert str(error) == "duplicate JSON object key"
        else:
            raise AssertionError("duplicate credential JSON was accepted")
        assert session_path.read_bytes() == before
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()


def test_maintenance_revocation_precedes_schema_56_to_current_work_and_survives_refusal() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, original = seed(root, schema=56)
        result = prepare_maintenance_revocation(root, check_runtime=False)
        assert result["autoResumeBlocked"] is True
        marker_before = (root / REVOCATION_FILE).read_bytes()
        try:
            activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
        except RuntimeError as error:
            assert "schema 56/66" in str(error)
        else:
            raise AssertionError("schema 56 unexpectedly activated")
        assert (root / REVOCATION_FILE).read_bytes() == marker_before
        assert not (root / POLICY_FILE).exists()
        assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_live_cortex_instance_lock_refuses_activation_without_touching_state() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, original = seed(root)
        with acquire_cortex_lock(root):
            try:
                activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
            except RuntimeError as error:
                assert "cortex.lock" in str(error) or "app or importer" in str(error)
            else:
                raise AssertionError("activation raced a live app/importer lock")
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()
        assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_missing_or_wrong_focus_is_refused_before_activation_mutates_state() -> None:
    for replacement, expected in (
        (None, "is required"),
        ({"segment_ids": ["focus-a", "focus-b"]}, "expected exactly 3"),
        ({"segment_ids": ["focus-a", "focus-b", "focus-wrong"]}, "digest mismatch"),
    ):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            db_path, original = seed(root)
            focus = root / "voice_focus.json"
            if replacement is None:
                focus.unlink()
            else:
                focus.write_text(json.dumps(replacement), encoding="utf-8")
            try:
                activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
            except RuntimeError as error:
                assert expected in str(error), error
            else:
                raise AssertionError("activation accepted a missing or wrong focus")
            assert not (root / POLICY_FILE).exists()
            assert not (root / REVOCATION_FILE).exists()
            assert json.loads((root / SESSION_FILE).read_text(encoding="utf-8")) == original


def test_focus_is_rechecked_immediately_before_promotion_and_commit() -> None:
    for drift_on_call, promoted in ((2, False), (3, True)):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            db_path, _ = seed(root)
            calls = 0

            def verify_then_drift(data_dir: Path):
                nonlocal calls
                calls += 1
                if calls == drift_on_call:
                    (data_dir / "voice_focus.json").write_text(
                        json.dumps({"segment_ids": ["focus-a", "focus-b", "focus-wrong"]}),
                        encoding="utf-8",
                    )
                return _verify_test_focus(data_dir)

            with mock.patch.object(activator, "verify_controlled_pilot_focus", side_effect=verify_then_drift):
                try:
                    activator.activate(root, db_path, expected_max_review_event_id=863, check_runtime=False)
                except RuntimeError as error:
                    assert "digest mismatch" in str(error), error
                else:
                    raise AssertionError("a focus drift escaped the activation recheck")
            assert calls == drift_on_call
            assert (root / REVOCATION_FILE).is_file(), "interrupted activation must remain paused"
            assert (root / POLICY_FILE).is_file() is promoted


def test_policy_contract_rejects_unknown_fields_and_python_bool_in_integer_slots() -> None:
    valid = pilot_policy(863)
    for mutate in (
        lambda value: value.update(typo=True),
        lambda value: value.update(schema_version=True),
        lambda value: value.update(after_review_event_id=True),
        lambda value: value.update(max_total_corpus_actions=True),
        lambda value: value["reviewers"][0].update(max_corpus_actions=True),
        lambda value: value["reviewers"][0].update(typo=True),
    ):
        broken = json.loads(json.dumps(valid))
        mutate(broken)
        try:
            validate_pilot_policy(broken)
        except RuntimeError:
            pass
        else:
            raise AssertionError(f"non-serde pilot policy was accepted: {broken}")

    try:
        pilot_policy(True)
    except RuntimeError:
        pass
    else:
        raise AssertionError("Python bool was accepted as a review-event baseline")


def test_tracked_focus_contract_and_8277_8279_wrong_id_failures_are_exact() -> None:
    production = load_pilot_focus_contract()
    assert production.segment_id_count == 8_278
    assert (
        production.sorted_unique_segment_ids_sha256
        == "9f7876c04ee7add77673f938460a5631056712b35a156c0d76b0cd7dca7ef3a7"
    )

    baseline = [f"segment-{index:05}" for index in range(8_278)]
    contract = contract_for_ids(baseline)
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        focus = root / "voice_focus.json"

        def expect_refusal(ids: list[str], expected: str) -> None:
            focus.write_text(json.dumps({"segment_ids": ids}), encoding="utf-8")
            try:
                verify_controlled_pilot_focus(root, contract)
            except PilotFocusError as error:
                assert expected in str(error), error
            else:
                raise AssertionError(f"focus variation was accepted: {expected}")

        expect_refusal(baseline[:-1], "8277")
        expect_refusal([*baseline, "segment-extra"], "8279")
        expect_refusal([*baseline[:-1], "segment-wrong"], "digest mismatch")


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"REVIEW PILOT ACTIVATION: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
