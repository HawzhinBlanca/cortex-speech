#!/usr/bin/env python3
"""Regression tests for the headless recovery-snapshot writer."""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import tempfile
from pathlib import Path
from typing import Any
from unittest import mock

from pilot_focus_contract import contract_for_ids, verify_controlled_pilot_focus

SCRIPT = Path(__file__).with_name("create_recovery_snapshot.py")
SPEC = importlib.util.spec_from_file_location("create_recovery_snapshot", SCRIPT)
assert SPEC and SPEC.loader
snapshot = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(snapshot)
TEST_FOCUS_CONTRACT = contract_for_ids(["s1"])
snapshot.verify_controlled_pilot_focus = lambda root: verify_controlled_pilot_focus(root, TEST_FOCUS_CONTRACT)


def seed_profile(root: Path) -> None:
    con = sqlite3.connect(root / "cortex-speech.db")
    con.executescript(
        """
        PRAGMA foreign_keys=ON;
        CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);
        CREATE TABLE speech_segments(id TEXT PRIMARY KEY);
        INSERT INTO speech_segments VALUES('s1');
        CREATE TABLE review_events(id INTEGER PRIMARY KEY);
        CREATE TABLE spot_checks(id INTEGER PRIMARY KEY);
        CREATE TABLE model_versions(id TEXT PRIMARY KEY);
        CREATE TABLE import_jobs(id TEXT PRIMARY KEY);
        CREATE TABLE import_job_files(id INTEGER PRIMARY KEY);
        """
    )
    con.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        snapshot.source_migrations(snapshot.DEFAULT_MIGRATIONS),
    )
    con.commit()
    con.close()
    (root / "settings.json").write_text(json.dumps({"backup_second_dir": ""}), encoding="utf-8")
    (root / "champion.json").write_text("{}", encoding="utf-8")
    (root / "reviewer_dialects.json").write_text("{}", encoding="utf-8")
    (root / "voice_focus.json").write_text('{"name":"x","segment_ids":["s1"]}', encoding="utf-8")
    (root / "review_pilot_policy.json").write_text(
        '{"schema_version":1,"after_review_event_id":0,"max_total_corpus_actions":20,'
        '"reviewers":[{"name":"Hawzhin","max_corpus_actions":10},'
        '{"name":"Pavel","max_corpus_actions":10}]}',
        encoding="utf-8",
    )


def promoted_fixture(base: Path) -> tuple[Path, dict[str, object]]:
    data = base / "data"
    data.mkdir()
    seed_profile(data)
    local, evidence = snapshot.promote_snapshot(
        data, label="strict", expected_foreign_keys=0, repo_root=base
    )
    return local, evidence


def load_manifest(tree: Path) -> dict[str, Any]:
    return json.loads((tree / snapshot.MANIFEST_FILE).read_text(encoding="utf-8"))


def write_manifest(tree: Path, value: dict[str, Any]) -> None:
    (tree / snapshot.MANIFEST_FILE).write_text(
        json.dumps(value, indent=2) + "\n", encoding="utf-8", newline="\n"
    )


def assert_verify_refuses(tree: Path, evidence: dict[str, object], expected: str) -> None:
    try:
        snapshot.verify_tree(tree, expected_evidence=evidence, expected_foreign_keys=0)
    except RuntimeError as error:
        assert expected in str(error), error
    else:
        raise AssertionError("malformed schema-2 snapshot unexpectedly verified")


def test_snapshot_and_offsite_copy_are_manifest_verified() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        offsite = base / "offsite"
        data.mkdir()
        seed_profile(data)
        local, evidence = snapshot.promote_snapshot(
            data, label="prechange", expected_foreign_keys=0, repo_root=base
        )
        remote = snapshot.mirror_offsite(local, offsite, evidence=evidence, expected_foreign_keys=0)
        for tree in (local, remote):
            assert (tree / snapshot.MANIFEST_FILE).is_file()
            assert (tree / "review_pilot_policy.json").is_file()
            assert not (tree / snapshot.REVIEW_PILOT_ABSENT_FILE).exists()
            snapshot.verify_tree(tree, expected_evidence=evidence, expected_foreign_keys=0)


def test_wal_mode_snapshot_verification_never_creates_unlisted_sidecars() -> None:
    """A production WAL database must remain an exact manifest tree after self-verification."""

    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        offsite = base / "offsite"
        data.mkdir()
        seed_profile(data)
        con = sqlite3.connect(data / snapshot.DB_FILE)
        try:
            assert con.execute("PRAGMA journal_mode=WAL").fetchone()[0].lower() == "wal"
        finally:
            con.close()

        local, evidence = snapshot.promote_snapshot(
            data, label="wal_mode", expected_foreign_keys=0, repo_root=base
        )
        assert not (local / f"{snapshot.DB_FILE}-wal").exists()
        assert not (local / f"{snapshot.DB_FILE}-shm").exists()
        con = snapshot.open_readonly(local / snapshot.DB_FILE, immutable=True)
        try:
            assert con.execute("PRAGMA journal_mode").fetchone()[0].lower() == "delete"
        finally:
            con.close()

        remote = snapshot.mirror_offsite(local, offsite, evidence=evidence, expected_foreign_keys=0)
        for tree in (local, remote):
            assert not (tree / f"{snapshot.DB_FILE}-wal").exists()
            assert not (tree / f"{snapshot.DB_FILE}-shm").exists()
            snapshot.verify_tree(tree, expected_evidence=evidence, expected_foreign_keys=0)


def test_post_verification_inventory_check_refuses_a_verifier_side_effect() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        real_db_evidence = snapshot.db_evidence

        def polluting_db_evidence(path: Path, *, immutable: bool = False) -> dict[str, object]:
            evidence = real_db_evidence(path, immutable=immutable)
            if immutable:
                (path.parent / f"{snapshot.DB_FILE}-wal").write_bytes(b"unexpected verifier output")
            return evidence

        with mock.patch.object(snapshot, "db_evidence", side_effect=polluting_db_evidence):
            try:
                snapshot.promote_snapshot(
                    data, label="polluted", expected_foreign_keys=0, repo_root=base
                )
            except RuntimeError as error:
                assert "unlisted" in str(error), error
            else:
                raise AssertionError("a verifier-created sidecar must block snapshot promotion")
        assert not list((data / "snapshots" / "pinned").glob("polluted_*"))


def test_wrong_expected_fk_count_refuses_before_promotion() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        try:
            snapshot.promote_snapshot(data, label="bad", expected_foreign_keys=1, repo_root=base)
        except RuntimeError as error:
            assert "foreign-key count" in str(error)
        else:
            raise AssertionError("wrong FK expectation must refuse")
        assert not list((data / "snapshots" / "pinned").glob("bad_*"))


def test_snapshot_records_and_verifies_explicit_pilot_absence() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        (data / snapshot.REVIEW_PILOT_FILE).unlink()
        local, evidence = snapshot.promote_snapshot(
            data, label="no_pilot", expected_foreign_keys=0, repo_root=base
        )
        assert not (local / snapshot.REVIEW_PILOT_FILE).exists()
        assert (local / snapshot.REVIEW_PILOT_ABSENT_FILE).read_bytes() == snapshot.REVIEW_PILOT_ABSENT_BYTES
        snapshot.verify_tree(local, expected_evidence=evidence, expected_foreign_keys=0)


def test_invalid_pilot_policy_refuses_snapshot_promotion() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        (data / snapshot.REVIEW_PILOT_FILE).write_text(
            '{"schema_version":1,"after_review_event_id":0,"max_total_corpus_actions":200,'
            '"reviewers":[{"name":"A","max_corpus_actions":100},'
            '{"name":"B","max_corpus_actions":100}]}',
            encoding="utf-8",
        )
        try:
            snapshot.promote_snapshot(data, label="unsafe_policy", expected_foreign_keys=0, repo_root=base)
        except RuntimeError as error:
            assert "exactly 20" in str(error)
        else:
            raise AssertionError("a weakened paid-review policy must never enter a recovery snapshot")
        assert not list((data / "snapshots" / "pinned").glob("unsafe_policy_*"))


def test_active_pilot_snapshot_refuses_missing_or_wrong_focus() -> None:
    for replacement, expected in (
        (None, "is required"),
        ({"segment_ids": ["wrong"]}, "digest mismatch"),
    ):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            data = base / "data"
            data.mkdir()
            seed_profile(data)
            focus = data / "voice_focus.json"
            if replacement is None:
                focus.unlink()
            else:
                focus.write_text(json.dumps(replacement), encoding="utf-8")
            try:
                snapshot.promote_snapshot(data, label="wrong_focus", expected_foreign_keys=0, repo_root=base)
            except RuntimeError as error:
                assert expected in str(error), error
            else:
                raise AssertionError("a policy-bearing snapshot accepted a missing or wrong focus")
            assert not list((data / "snapshots" / "pinned").glob("wrong_focus_*"))


def test_manifest_hashes_cannot_bless_a_different_controlled_pilot_focus() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tree, evidence = promoted_fixture(Path(raw))
        focus = tree / "voice_focus.json"
        focus.write_text(json.dumps({"segment_ids": ["wrong"]}), encoding="utf-8")
        manifest = load_manifest(tree)
        row = next(item for item in manifest["files"] if item["path"] == "voice_focus.json")
        row["sizeBytes"] = focus.stat().st_size
        row["sha256"] = snapshot.sha256_file(focus)
        write_manifest(tree, manifest)
        assert_verify_refuses(tree, evidence, "digest mismatch")


def test_legally_absent_state_is_captured_as_exact_markers() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        for name in snapshot.REQUIRED_STATE:
            (data / name).unlink()
        (data / snapshot.REVIEW_PILOT_FILE).unlink()
        local, evidence = snapshot.promote_snapshot(
            data, label="default_state", expected_foreign_keys=0, repo_root=base
        )
        declared = {row["path"] for row in load_manifest(local)["files"]}
        for name in snapshot.REQUIRED_STATE:
            marker = snapshot.state_absence_marker(name)
            assert name not in declared
            assert marker in declared
            assert (local / marker).read_bytes() == snapshot.state_absence_bytes(name)
        snapshot.verify_tree(local, expected_evidence=evidence, expected_foreign_keys=0)


def test_snapshot_refuses_restore_pending_and_live_lock_overlap_before_promotion() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        (data / snapshot.RESTORE_PENDING_FILE).write_text("pending\n", encoding="utf-8")
        try:
            snapshot.promote_snapshot(data, label="pending", expected_foreign_keys=0, repo_root=base)
        except RuntimeError as error:
            assert "interrupted restore is pending" in str(error)
        else:
            raise AssertionError("a restore-pending profile must not be snapshotted")
        assert not list((data / "snapshots" / "pinned").glob("pending_*"))

    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        with snapshot.acquire_cortex_lock(data):
            try:
                snapshot.promote_snapshot(data, label="overlap", expected_foreign_keys=0, repo_root=base)
            except RuntimeError as error:
                assert "cortex.lock" in str(error) or "app or importer" in str(error)
            else:
                raise AssertionError("a concurrent app/importer lock owner must block capture")
        assert not list((data / "snapshots" / "pinned").glob("overlap_*"))


def test_capture_lock_remains_held_through_pre_promotion_self_verification() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        original_verify = snapshot.verify_tree
        observed = False

        def verify_while_locked(*args: Any, **kwargs: Any) -> None:
            nonlocal observed
            try:
                with snapshot.acquire_cortex_lock(data):
                    pass
            except RuntimeError as error:
                assert "cortex.lock" in str(error) or "app or importer" in str(error)
                observed = True
            else:
                raise AssertionError("capture released cortex.lock before self-verification")
            original_verify(*args, **kwargs)

        with mock.patch.object(snapshot, "verify_tree", side_effect=verify_while_locked):
            local, _evidence = snapshot.promote_snapshot(
                data, label="locked_span", expected_foreign_keys=0, repo_root=base
            )
        assert observed
        assert local.is_dir()


def test_snapshot_requires_exact_description_bound_migration_prefix() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        connection = sqlite3.connect(data / snapshot.DB_FILE)
        connection.execute("DELETE FROM schema_migrations WHERE version > 57")
        connection.commit()
        connection.close()
        local, evidence = snapshot.promote_snapshot(
            data, label="older_prefix", expected_foreign_keys=0, repo_root=base
        )
        assert evidence["schemaVersion"] == 57
        snapshot.verify_tree(local, expected_evidence=evidence, expected_foreign_keys=0)

    mutations = (
        ("DELETE FROM schema_migrations WHERE version <> 57", "missing="),
        ("DELETE FROM schema_migrations WHERE version = 23", "missing=[23]"),
        (
            "UPDATE schema_migrations SET description = 'tampered' WHERE version = 31",
            "descriptionMismatch=[31]",
        ),
        ("DROP TABLE schema_migrations", "history cannot be read"),
        (
            "INSERT INTO schema_migrations(version, description) VALUES(99999, 'future')",
            "newer than this source supports",
        ),
    )
    for statement, expected in mutations:
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            data = base / "data"
            data.mkdir()
            seed_profile(data)
            connection = sqlite3.connect(data / snapshot.DB_FILE)
            connection.execute(statement)
            connection.commit()
            connection.close()
            try:
                snapshot.promote_snapshot(
                    data, label="bad_history", expected_foreign_keys=0, repo_root=base
                )
            except RuntimeError as error:
                assert expected in str(error), error
            else:
                raise AssertionError("snapshot with altered migration history must be refused")
            assert not list((data / "snapshots" / "pinned").glob("bad_history_*"))


def test_verify_tree_rejects_duplicate_and_nonexact_manifest_shapes() -> None:
    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        manifest_path = local / snapshot.MANIFEST_FILE
        original = manifest_path.read_text(encoding="utf-8")

        manifest_path.write_text(
            original.replace('"schema": 2', '"schema": 2, "schema": 2', 1), encoding="utf-8"
        )
        assert_verify_refuses(local, evidence, "duplicate JSON object key")

        manifest_path.write_text(original, encoding="utf-8")
        payload = load_manifest(local)
        payload["unexpected"] = True
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "extra=['unexpected']")

        manifest_path.write_text(original, encoding="utf-8")
        payload = load_manifest(local)
        payload["files"][0]["unexpected"] = True
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "file row 0 fields are invalid")


def test_verify_tree_rejects_unsafe_duplicate_and_nonexact_inventory() -> None:
    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        manifest_path = local / snapshot.MANIFEST_FILE
        original = manifest_path.read_bytes()

        payload = load_manifest(local)
        payload["files"][0]["path"] = "../escape"
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "unsafe file path")

        manifest_path.write_bytes(original)
        payload = load_manifest(local)
        payload["files"].append(dict(payload["files"][0]))
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "duplicate file")

        manifest_path.write_bytes(original)
        extra = local / "unlisted.txt"
        extra.write_text("not inventoried", encoding="utf-8")
        assert_verify_refuses(local, evidence, "unlisted=['unlisted.txt']")


def test_verify_tree_requires_one_exact_representation_for_state_and_pilot() -> None:
    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        payload = load_manifest(local)
        (local / "reviewer_dialects.json").unlink()
        payload["files"] = [
            row for row in payload["files"] if row["path"] != "reviewer_dialects.json"
        ]
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "reviewer_dialects.json")

    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        name = "voice_focus.json"
        marker = local / snapshot.state_absence_marker(name)
        marker.write_bytes(snapshot.state_absence_bytes(name))
        payload = load_manifest(local)
        payload["files"].append(
            {
                "path": marker.name,
                "sizeBytes": marker.stat().st_size,
                "sha256": snapshot.sha256_file(marker),
            }
        )
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "exactly one")

    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        name = "settings.json"
        (data / name).unlink()
        local, evidence = snapshot.promote_snapshot(
            data, label="bad_marker", expected_foreign_keys=0, repo_root=base
        )
        marker = local / snapshot.state_absence_marker(name)
        marker.write_bytes(b"wrong\n")
        payload = load_manifest(local)
        for row in payload["files"]:
            if row["path"] == marker.name:
                row["sizeBytes"] = marker.stat().st_size
                row["sha256"] = snapshot.sha256_file(marker)
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "invalid contents")

    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        marker = local / snapshot.REVIEW_PILOT_ABSENT_FILE
        marker.write_bytes(snapshot.REVIEW_PILOT_ABSENT_BYTES)
        payload = load_manifest(local)
        payload["files"].append(
            {
                "path": marker.name,
                "sizeBytes": marker.stat().st_size,
                "sha256": snapshot.sha256_file(marker),
            }
        )
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "exactly one")


def test_verify_tree_binds_schema2_evidence_and_policy_baseline() -> None:
    with tempfile.TemporaryDirectory() as raw:
        local, evidence = promoted_fixture(Path(raw))
        payload = load_manifest(local)
        payload["databaseEvidence"]["schemaVersion"] = 55
        write_manifest(local, payload)
        assert_verify_refuses(local, evidence, "databaseEvidence differs")

    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        policy = json.loads((data / snapshot.REVIEW_PILOT_FILE).read_text(encoding="utf-8"))
        policy["after_review_event_id"] = 1
        (data / snapshot.REVIEW_PILOT_FILE).write_text(json.dumps(policy), encoding="utf-8")
        try:
            snapshot.promote_snapshot(data, label="ahead", expected_foreign_keys=0, repo_root=base)
        except RuntimeError as error:
            assert "ahead of its database review-event maximum" in str(error)
        else:
            raise AssertionError("a pilot baseline ahead of the restored DB must be refused")


def test_offsite_staging_cleanup_never_deletes_a_preexisting_path() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        offsite = base / "offsite"
        data.mkdir()
        seed_profile(data)
        local, evidence = snapshot.promote_snapshot(
            data, label="prechange", expected_foreign_keys=0, repo_root=base
        )
        remote_root = offsite / "snapshots" / "pinned"
        remote_root.mkdir(parents=True)
        old_predictable_staging = remote_root / f".{local.name}.staging-{snapshot.os.getpid()}"
        old_predictable_staging.mkdir()
        sentinel = old_predictable_staging / "belongs-to-someone-else.txt"
        sentinel.write_text("preserve", encoding="utf-8")

        with mock.patch.object(snapshot, "verify_tree", side_effect=RuntimeError("forced verification failure")):
            try:
                snapshot.mirror_offsite(local, offsite, evidence=evidence, expected_foreign_keys=0)
            except RuntimeError as error:
                assert "forced verification failure" in str(error)
            else:
                raise AssertionError("forced verification failure must abort the mirror")

        assert sentinel.read_text(encoding="utf-8") == "preserve"
        assert not (remote_root / local.name).exists()
        owned_staging = [
            path
            for path in remote_root.iterdir()
            if path.name.startswith(f".{local.name}.staging-") and path != old_predictable_staging
        ]
        assert owned_staging == []


def test_offsite_overlap_is_rejected_before_any_write() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data = base / "data"
        data.mkdir()
        seed_profile(data)
        local, evidence = snapshot.promote_snapshot(
            data, label="prechange", expected_foreign_keys=0, repo_root=base
        )
        nested = local / "must-not-be-created"
        for unsafe in (local, nested, data):
            try:
                snapshot.mirror_offsite(local, unsafe, evidence=evidence, expected_foreign_keys=0)
            except RuntimeError as error:
                assert "overlaps" in str(error)
            else:
                raise AssertionError(f"overlapping offsite path must be refused: {unsafe}")
        assert not nested.exists()
        assert not (local / "snapshots").exists()


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"RECOVERY SNAPSHOT: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
