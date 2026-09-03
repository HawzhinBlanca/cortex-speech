#!/usr/bin/env python3
"""Regression tests for the fail-closed Rezan-only first-pass transition."""

from __future__ import annotations

import json
import sqlite3
import tempfile
from pathlib import Path

from activate_review_pilot import POLICY_FILE, REVOCATION_FILE, SESSION_FILE, pilot_policy, sha256_file
from activate_sequential_review import (
    CAMPAIGN_SETTINGS_KEY,
    REVIEWER,
    activate,
    campaign_policy,
    inspect,
)
from check_database_integrity import DEFAULT_MIGRATIONS, source_migrations
from pilot_focus_contract import focus_evidence, load_voice_focus_ids


def seed(root: Path, *, foreign_reviewer: bool = False) -> tuple[Path, dict[str, object]]:
    db_path = root / "cortex-speech.db"
    conn = sqlite3.connect(db_path)
    conn.executescript(
        """
        PRAGMA foreign_keys=ON;
        CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);
        CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE review_events(
            id INTEGER PRIMARY KEY, segment_id TEXT NOT NULL, reviewer TEXT NOT NULL,
            action TEXT NOT NULL, source TEXT NOT NULL
        );
        CREATE TABLE review_compensation_policies(policy_version TEXT PRIMARY KEY);
        INSERT INTO review_compensation_policies VALUES('review-iqd-v1-2026-08-21');
        INSERT INTO review_events VALUES(863, 'legacy', 'legacy', 'accept', 'couch');
        INSERT INTO review_events VALUES(864, 'work-a', 'Rezan', 'edit', 'couch');
        INSERT INTO review_events VALUES(865, 'hidden-a', 'Rezan', 'accept', 'couch_spot_check');
        """
    )
    if foreign_reviewer:
        conn.execute("UPDATE review_events SET reviewer='Aram' WHERE id=864")
    conn.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        source_migrations(DEFAULT_MIGRATIONS),
    )
    conn.commit()
    conn.close()

    policy = pilot_policy(863)
    (root / POLICY_FILE).write_text(json.dumps(policy), encoding="utf-8")
    session: dict[str, object] = {
        "reviewers": {"protected-rezan": "Rezan", "protected-aram": "Aram"},
        "db_path": str(db_path),
        "spot_checks": [["hidden-a", "Rezan"], ["hidden-b", "Aram"]],
        "pilot_spot_checks": [["hidden-a", "Rezan"], ["hidden-b", "Aram"]],
        "sessions": [
            {"token": "cookie-r", "reviewer": "Rezan", "issued_unix": 1},
            {"token": "cookie-a", "reviewer": "Aram", "issued_unix": 2},
        ],
        "pilot_policy": policy,
    }
    (root / SESSION_FILE).write_text(json.dumps(session), encoding="utf-8")
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "test", "segment_ids": ["work-a", "work-b"]}), encoding="utf-8"
    )
    return db_path, session


def test_activation_preserves_history_and_token_but_retires_every_pilot_surface() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw).resolve()
        db_path, original_session = seed(root)
        pilot_hash = sha256_file(root / POLICY_FILE)
        before_conn = sqlite3.connect(db_path)
        before = before_conn.execute(
            "SELECT id, segment_id, reviewer, action, source FROM review_events ORDER BY id"
        ).fetchall()
        before_conn.close()

        result = activate(
            root,
            db_path,
            expected_max_review_event_id=865,
            expected_pilot_policy_sha256=pilot_hash,
            check_runtime=False,
        )

        session = json.loads((root / SESSION_FILE).read_text(encoding="utf-8"))
        assert session["reviewers"] == {"protected-rezan": REVIEWER}
        assert session["sessions"] == []
        assert session["spot_checks"] == []
        assert session["pilot_spot_checks"] == []
        assert session["pilot_policy"] is None
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()
        assert result["retainedCorpusEvents"] == 1
        assert result["retainedInvalidPilotChecks"] == 1
        assert result["exportsBlockedPendingIndependentSecondPass"] is True

        conn = sqlite3.connect(db_path)
        after = conn.execute(
            "SELECT id, segment_id, reviewer, action, source FROM review_events ORDER BY id"
        ).fetchall()
        stored = json.loads(conn.execute(
            "SELECT value FROM settings WHERE key=?", (CAMPAIGN_SETTINGS_KEY,)
        ).fetchone()[0])
        conn.close()
        assert after == before, "activation must never rewrite review/payment source events"
        assert stored == result["campaign"]
        assert stored["after_review_event_id"] == 863
        assert stored["activated_at_review_event_id"] == 865
        assert stored["provisional_export_block"] is True

        backup = Path(result["backup"])
        assert json.loads((backup / SESSION_FILE).read_text(encoding="utf-8")) == original_session
        assert (backup / "cortex-speech.db").is_file()
        assert inspect(root, db_path)["campaign"] == stored


def test_cas_and_foreign_reviewer_fail_before_any_live_mutation() -> None:
    for foreign_reviewer, expected_error in ((False, "CAS mismatch"), (True, "Rezan-only takeover")):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            db_path, _ = seed(root, foreign_reviewer=foreign_reviewer)
            pilot_path = root / POLICY_FILE
            session_before = (root / SESSION_FILE).read_bytes()
            try:
                activate(
                    root,
                    db_path,
                    expected_max_review_event_id=864 if not foreign_reviewer else 865,
                    expected_pilot_policy_sha256=sha256_file(pilot_path),
                    check_runtime=False,
                )
            except RuntimeError as error:
                assert expected_error in str(error), error
            else:
                raise AssertionError("unsafe sequential activation was accepted")
            assert pilot_path.is_file()
            assert (root / SESSION_FILE).read_bytes() == session_before
            assert not (root / REVOCATION_FILE).exists()
            conn = sqlite3.connect(db_path)
            assert conn.execute(
                "SELECT COUNT(*) FROM settings WHERE key=?", (CAMPAIGN_SETTINGS_KEY,)
            ).fetchone()[0] == 0
            conn.close()


def test_interruption_between_session_promotion_and_pilot_retirement_resumes_safely() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw).resolve()
        db_path, _ = seed(root)
        pilot_hash = sha256_file(root / POLICY_FILE)
        focus = focus_evidence(load_voice_focus_ids(root))
        policy = campaign_policy(
            baseline=863,
            activation_max=865,
            focus_count=focus.segment_id_count,
            focus_sha256=focus.sorted_unique_segment_ids_sha256,
        )
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?, ?)",
            (CAMPAIGN_SETTINGS_KEY, json.dumps(policy, separators=(",", ":"), sort_keys=True)),
        )
        conn.commit()
        conn.close()
        session = json.loads((root / SESSION_FILE).read_text(encoding="utf-8"))
        session.update({
            "reviewers": {"protected-rezan": "Rezan"},
            "sessions": [],
            "spot_checks": [],
            "pilot_spot_checks": [],
            "pilot_policy": None,
        })
        (root / SESSION_FILE).write_text(json.dumps(session), encoding="utf-8")
        backup = root / "sequential_activation_backups" / "interrupted"
        backup.mkdir(parents=True)
        (backup / "ACTIVATION_BACKUP.json").write_text("{}", encoding="utf-8")
        (root / REVOCATION_FILE).write_text(json.dumps({
            "reason": "sequential_review_activation",
            "backup": str(backup),
            "campaignId": policy["campaign_id"],
        }), encoding="utf-8")

        result = activate(
            root,
            db_path,
            expected_max_review_event_id=865,
            expected_pilot_policy_sha256=pilot_hash,
            check_runtime=False,
        )
        assert result["campaign"] == policy
        assert not (root / POLICY_FILE).exists()
        assert not (root / REVOCATION_FILE).exists()


def main() -> int:
    tests = (
        test_activation_preserves_history_and_token_but_retires_every_pilot_surface,
        test_cas_and_foreign_reviewer_fail_before_any_live_mutation,
        test_interruption_between_session_promotion_and_pilot_retirement_resumes_safely,
    )
    for test in tests:
        test()
        print(f"ok  {test.__name__}")
    print("sequential review activation tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
