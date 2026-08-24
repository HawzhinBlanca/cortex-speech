#!/usr/bin/env python3
"""Regression tests for the atomic completed-import named-voice transition."""

from __future__ import annotations

import json
import sqlite3
import tempfile
from pathlib import Path

from activate_named_voice_focus import activate
from activate_review_pilot import REVOCATION_FILE, SESSION_FILE, sha256_file
from activate_sequential_review import CAMPAIGN_SETTINGS_KEY, campaign_policy
from check_database_integrity import DEFAULT_MIGRATIONS, source_migrations
from pilot_focus_contract import focus_evidence, load_voice_focus_ids


JOB_ID = "11111111-1111-1111-1111-111111111111"
MODEL_ID = "omniasr-7b-test"
MODEL_SHA256 = "c" * 64
OLD_FOCUS = {"lamo-a", "lamo-b", "other-c"}
TARGET = {"lamo-a", "lamo-b"}


def seed(root: Path) -> tuple[Path, Path, dict[str, object]]:
    source = root / "lamo-wavs"
    source.mkdir()
    paths: dict[str, Path] = {}
    for segment_id in sorted(TARGET):
        path = source / f"{segment_id}.wav"
        path.write_bytes(b"RIFF-test")
        paths[segment_id] = path

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
        CREATE TABLE model_versions(
            id TEXT PRIMARY KEY, family TEXT NOT NULL,
            checkpoint_sha256 TEXT NOT NULL, status TEXT NOT NULL
        );
        CREATE TABLE review_compensation_policies(policy_version TEXT PRIMARY KEY);
        INSERT INTO review_compensation_policies VALUES('review-iqd-v1-2026-08-21');
        INSERT INTO review_events VALUES(875, 'legacy', 'Rubar', 'edit', 'couch');
        CREATE TABLE import_jobs(id TEXT PRIMARY KEY, dir TEXT NOT NULL, total_files INTEGER NOT NULL, status TEXT NOT NULL);
        CREATE TABLE import_job_files(job_id TEXT NOT NULL, path TEXT NOT NULL);
        CREATE TABLE speech_segments(
            id TEXT PRIMARY KEY, audio_path TEXT NOT NULL, raw_transcript TEXT,
            model_version_id TEXT, speaker_id TEXT, review_revision INTEGER,
            verified INTEGER NOT NULL DEFAULT 0
        );
        """
    )
    conn.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        source_migrations(DEFAULT_MIGRATIONS),
    )
    conn.execute(
        "INSERT INTO model_versions VALUES(?, 'omniasr-7b', ?, 'champion')", (MODEL_ID, MODEL_SHA256)
    )
    conn.execute("INSERT INTO import_jobs VALUES(?, ?, 2, 'completed')", (JOB_ID, str(source)))
    for segment_id, path in paths.items():
        conn.execute("INSERT INTO import_job_files VALUES(?, ?)", (JOB_ID, str(path)))
        conn.execute(
            "INSERT INTO speech_segments VALUES(?, ?, ?, ?, 'SPEAKER_00', NULL, 0)",
            (segment_id, str(path), f"draft {segment_id}", MODEL_ID),
        )
    conn.execute(
        "INSERT INTO speech_segments VALUES('other-c', 'other.wav', 'other draft', ?, 'Other', 7, 0)",
        (MODEL_ID,),
    )
    old_evidence = focus_evidence(OLD_FOCUS)
    old_campaign = campaign_policy(
        baseline=863,
        activation_max=875,
        focus_count=old_evidence.segment_id_count,
        focus_sha256=old_evidence.sorted_unique_segment_ids_sha256,
    )
    conn.execute(
        "INSERT INTO settings VALUES(?, ?)",
        (CAMPAIGN_SETTINGS_KEY, json.dumps(old_campaign, separators=(",", ":"), sort_keys=True)),
    )
    conn.commit()
    conn.close()

    session: dict[str, object] = {
        "reviewers": {"protected-rubar": "Rubar"},
        "db_path": str(db_path),
        "spot_checks": [],
        "pilot_spot_checks": [],
        "sessions": [{"token": "cookie-r", "reviewer": "Rubar", "issued_unix": 1}],
        "pilot_policy": None,
    }
    (root / SESSION_FILE).write_text(json.dumps(session), encoding="utf-8")
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "mixed", "activated_at": "old", "segment_ids": sorted(OLD_FOCUS)}),
        encoding="utf-8",
    )
    # Deliberately STALE: champion.json is the startup mirror the app rewrites on every launch, so in
    # the register-first/restart-second window it names a model the registry no longer champions.
    # Activation must read model_versions instead — a fixture that agreed with the mirror could not
    # tell the two sources apart.
    (root / "champion.json").write_text(
        json.dumps({"champions": {"omniasr-7b": {"modelVersionId": "omniasr-7b-stale-mirror"}}}),
        encoding="utf-8",
    )
    return db_path, source, old_campaign


def test_success_is_exact_history_preserving_and_session_byte_preserving() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, source, old_campaign = seed(root)
        session_hash = sha256_file(root / SESSION_FILE)
        result = activate(
            root,
            db_path,
            speaker_name="Lamo",
            import_job_id=JOB_ID,
            expected_current_campaign_id=str(old_campaign["campaign_id"]),
            expected_max_review_event_id=875,
            expected_source_dir=source,
            check_runtime=False,
        )

        assert result["clips"] == 2
        assert result["transcriptionRun"] is False
        assert result["gpuTouched"] is False
        assert sha256_file(root / SESSION_FILE) == session_hash
        assert load_voice_focus_ids(root) == TARGET
        focus = json.loads((root / "voice_focus.json").read_text(encoding="utf-8"))
        assert focus["name"] == "Lamo" and focus["activated_at"] != "old"
        assert not (root / REVOCATION_FILE).exists()

        conn = sqlite3.connect(db_path)
        rows = conn.execute(
            "SELECT id, speaker_id, review_revision FROM speech_segments ORDER BY id"
        ).fetchall()
        events = conn.execute("SELECT * FROM review_events ORDER BY id").fetchall()
        campaign = json.loads(
            conn.execute("SELECT value FROM settings WHERE key=?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()[0]
        )
        conn.close()
        assert rows == [("lamo-a", "Lamo", 1), ("lamo-b", "Lamo", 1), ("other-c", "Other", 7)]
        assert events == [(875, "legacy", "Rubar", "edit", "couch")]
        assert campaign == result["campaign"]
        assert campaign["focus_segment_count"] == 2
        assert Path(str(result["backup"]), "ACTIVATION_BACKUP.json").is_file()


def test_stale_event_boundary_fails_before_live_mutation() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, source, old_campaign = seed(root)
        before_db = sha256_file(db_path)
        before_focus = (root / "voice_focus.json").read_bytes()
        try:
            activate(
                root,
                db_path,
                speaker_name="Lamo",
                import_job_id=JOB_ID,
                expected_current_campaign_id=str(old_campaign["campaign_id"]),
                expected_max_review_event_id=874,
                expected_source_dir=source,
                check_runtime=False,
            )
        except RuntimeError as error:
            assert "review-event CAS mismatch" in str(error)
        else:
            raise AssertionError("stale event boundary was accepted")
        assert sha256_file(db_path) == before_db
        assert (root / "voice_focus.json").read_bytes() == before_focus
        assert not (root / REVOCATION_FILE).exists()


def test_interruption_after_database_commit_resumes_without_double_revision() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, source, old_campaign = seed(root)
        kwargs = dict(
            speaker_name="Lamo",
            import_job_id=JOB_ID,
            expected_current_campaign_id=str(old_campaign["campaign_id"]),
            expected_max_review_event_id=875,
            expected_source_dir=source,
            check_runtime=False,
        )
        try:
            activate(root, db_path, fail_after_db_commit_for_test=True, **kwargs)
        except RuntimeError as error:
            assert "injected failure" in str(error)
        else:
            raise AssertionError("injected activation failure did not fire")
        assert (root / REVOCATION_FILE).is_file()
        assert load_voice_focus_ids(root) == OLD_FOCUS

        result = activate(root, db_path, **kwargs)
        assert load_voice_focus_ids(root) == TARGET
        assert not (root / REVOCATION_FILE).exists()
        conn = sqlite3.connect(db_path)
        revisions = conn.execute(
            "SELECT review_revision FROM speech_segments WHERE id IN ('lamo-a', 'lamo-b') ORDER BY id"
        ).fetchall()
        conn.close()
        assert revisions == [(1,), (1,)]
        assert result["clips"] == 2


def test_registry_not_the_startup_mirror_decides_which_drafts_are_champion() -> None:
    """A registry with no usable champion refuses; the stale mirror never rescues it."""
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path, source, old_campaign = seed(root)
        conn = sqlite3.connect(db_path)
        conn.execute("UPDATE model_versions SET status = 'rolled_back'")
        conn.commit()
        conn.close()
        before_db = sha256_file(db_path)
        try:
            activate(
                root,
                db_path,
                speaker_name="Lamo",
                import_job_id=JOB_ID,
                expected_current_campaign_id=str(old_campaign["campaign_id"]),
                expected_max_review_event_id=875,
                expected_source_dir=source,
                check_runtime=False,
            )
        except RuntimeError as error:
            assert "exactly one omniasr-7b champion" in str(error), error
        else:
            raise AssertionError("an unresolvable registry champion was accepted")
        assert sha256_file(db_path) == before_db
        assert load_voice_focus_ids(root) == OLD_FOCUS


if __name__ == "__main__":
    test_success_is_exact_history_preserving_and_session_byte_preserving()
    test_stale_event_boundary_fails_before_live_mutation()
    test_interruption_after_database_commit_resumes_without_double_revision()
    test_registry_not_the_startup_mirror_decides_which_drafts_are_champion()
    print("named-voice activation tests: PASS")
