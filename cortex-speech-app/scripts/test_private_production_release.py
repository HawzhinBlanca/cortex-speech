#!/usr/bin/env python3
"""Fail-before tests for immutable release staging and schema-safe rollback decisions."""

from __future__ import annotations

import ctypes
import hashlib
import importlib.util
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

import check_supervision_live as supervision
from _couch_policy_util import couch_surface

APP = Path(__file__).resolve().parent.parent
SUBJECT = APP / "scripts" / "release_private_production.py"
SPEC = importlib.util.spec_from_file_location("private_release", SUBJECT)
assert SPEC and SPEC.loader
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class QuietHealthHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler protocol method
        self.send_response(401)
        self.end_headers()

    def log_message(self, _format: str, *_args: object) -> None:
        return


def current_windows_process_image() -> Path:
    """Return the kernel's executable identity, not a venv/launcher alias.

    ``sys.executable`` can name a virtual-environment redirect while ``Get-Process.Path`` reports
    the underlying CPython image. The watchdog intentionally compares kernel process images, so its
    positive control must use that same authority or it can reject the true listener for the right
    reason and make the regression test look flaky.
    """
    buffer = ctypes.create_unicode_buffer(32_768)
    length = ctypes.windll.kernel32.GetModuleFileNameW(None, buffer, len(buffer))
    if length == 0 or length == len(buffer):
        raise ctypes.WinError()
    return Path(buffer.value)


def short_directory_launch_path(path: Path) -> str:
    """Return ``path`` reached through the 8.3 form of its directory, keeping the executable name.

    This is the identity Windows records for anything started under a shortened directory - a CI
    runner's shortened profile directory, a shortcut through ``C:\\PROGRA~1\\...``. Only the
    directory is shortened on purpose: the process keeps its real image name, so it is still the
    same process the stop path claims to find, and the only thing that differs is the path text.

    Volumes with 8.3 name creation disabled hand back the long directory unchanged; the caller's
    other assertions still hold there, that machine simply cannot stage this launch shape.
    """
    buffer = ctypes.create_unicode_buffer(32_768)
    length = ctypes.windll.kernel32.GetShortPathNameW(str(path.parent), buffer, len(buffer))
    if length == 0 or length >= len(buffer):
        raise ctypes.WinError()
    return str(Path(buffer.value) / path.name)


def seed_source(root: Path) -> None:
    scripts = root / "scripts"
    (scripts / "ops").mkdir(parents=True)
    (scripts / "ops" / "cortex-watchdog.ps1").write_text("Write-Output 'watchdog'\n", encoding="utf-8")
    (scripts / "release_private_production.py").write_text("# controller\n", encoding="utf-8")
    shutil.copy2(APP / "scripts" / release.SCHEMA_CONTRACT_FILE, scripts / release.SCHEMA_CONTRACT_FILE)
    shutil.copy2(
        APP / "scripts" / "append_only_migration_contract.v1.json",
        scripts / "append_only_migration_contract.v1.json",
    )
    migrations = root / "src-tauri" / "src" / "migrations"
    migrations.mkdir(parents=True)
    shutil.copy2(APP / "src-tauri" / "src" / "migrations" / "mod.rs", migrations / "mod.rs")
    shutil.copy2(APP / "src-tauri" / "src" / "dialect.rs", migrations.parent / "dialect.rs")
    dedup = {
        "manifestSchema": 1,
        "summary": {"unconfirmedRiskGroups": 0},
    }
    dedup["manifestSha256"] = hashlib.sha256(
        json.dumps(dedup, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    (root / release.DEDUP_MANIFEST_FILE).write_text(json.dumps(dedup), encoding="utf-8")


def seed_candidate(
    root: Path,
    app: bytes = b"candidate-app",
    admin: bytes = b"candidate-admin",
    git_sha: str = "a" * 40,
) -> None:
    root.mkdir(parents=True, exist_ok=True)
    marker = b"\0CORTEX_BUILD_SHA:" + git_sha.encode("ascii") + b"\0"
    (root / "cortex-speech-app.exe").write_bytes(app + marker)
    (root / "pool_admin.exe").write_bytes(admin + marker)


def seed_database(path: Path, version: int, marker: str = "test") -> None:
    connection = sqlite3.connect(path)
    connection.executescript(
        "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);"
        "CREATE TABLE review_pool_decisions(id INTEGER PRIMARY KEY);"
        "CREATE TABLE marker(value TEXT);"
    )
    connection.executemany(
        "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
        [(item, f"migration-{item}") for item in range(1, version + 1)],
    )
    connection.execute("INSERT INTO marker VALUES(?)", (marker,))
    connection.commit()
    connection.close()


def seal_snapshot(snapshot: Path) -> str:
    """Write the same closed schema-1 inventory contract accepted by restore_drill."""

    import restore_drill

    for name in restore_drill.REQUIRED_STATE:
        marker = snapshot / restore_drill.state_absence_marker(name)
        marker.write_bytes(restore_drill.state_absence_bytes(name))
    (snapshot / restore_drill.REVIEW_PILOT_ABSENT_FILE).write_bytes(
        restore_drill.REVIEW_PILOT_ABSENT_BYTES
    )
    rows = []
    for path in sorted(snapshot.iterdir(), key=lambda value: value.name):
        if path.name == restore_drill.MANIFEST:
            continue
        rows.append(
            {
                "path": path.name,
                "sizeBytes": path.stat().st_size,
                "sha256": release.sha256_file(path),
            }
        )
    release.atomic_json(
        snapshot / restore_drill.MANIFEST,
        {
            "schema": 1,
            "reviewPilotPolicyStateSchema": 1,
            "createdAtEpochSecs": 1,
            "appGitSha": "test-fixture",
            "files": rows,
        },
    )
    restore_drill.validate_snapshot_manifest(snapshot)
    return release.sha256_file(snapshot / restore_drill.MANIFEST)


def as_legacy_v65(manifest: dict[str, object]) -> dict[str, object]:
    legacy = {key: value for key, value in manifest.items() if key in release.LEGACY_V1_MANIFEST_FIELDS}
    legacy["schema"] = 1
    legacy["expectedDatabaseSchema"] = 65
    return legacy


def release_journal(
    candidate: dict[str, object],
    previous: dict[str, object] | None,
    *,
    source_schema: int,
    phase: str,
    snapshot: Path | None,
    target_digest: str | None,
    baseline: int = 0,
) -> dict[str, object]:
    return {
        "schema": 2,
        "phase": phase,
        "startedAtUtc": release.utc_now(),
        "sourceSchema": source_schema,
        "baselinePoolDecisionId": baseline,
        "candidate": candidate,
        "previousActive": previous,
        "fallbackApp": None,
        "fallbackWatchdog": None,
        "snapshotDir": str(snapshot) if snapshot else None,
        "snapshotManifestSha256": (
            release.sha256_file(snapshot / "SNAPSHOT_MANIFEST.json") if snapshot else None
        ),
        "targetDatabaseSha256": target_digest,
    }


def test_stage_is_atomic_versioned_and_hash_bound() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source = base / "source"
        candidate = base / "candidate"
        releases = base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="a" * 40)
        git_sha = "a" * 40
        manifest = release.stage_release(candidate, source, releases, git_sha)
        final = Path(manifest["directory"])
        assert final.name == (
            f"{git_sha[:12]}-{release.sha256_file(candidate / 'cortex-speech-app.exe')[:12]}-"
            f"{release.operations_bundle_sha256(source)[:12]}-{manifest['schemaContractSha256'][:12]}-"
            f"{manifest['dedupManifestSha256'][:12]}"
        )
        assert manifest["schema"] == 2
        assert manifest["expectedDatabaseSchema"] == 70
        assert manifest["schemaContractId"] == release.SCHEMA_CONTRACT_ID
        assert manifest["schemaContractSha256"] == release.sha256_file(Path(manifest["schemaContract"]))
        assert manifest["appSha256"] == release.sha256_file(candidate / "cortex-speech-app.exe")
        assert manifest["poolAdminSha256"] == release.sha256_file(candidate / "pool_admin.exe")
        assert (final / "src-tauri" / "src" / "dialect.rs").read_bytes() == (
            source / "src-tauri" / "src" / "dialect.rs"
        ).read_bytes()
        assert not list(releases.glob(".*.staging-*"))
        assert release.validate_manifest(
            json.loads((final / release.RELEASE_MANIFEST_FILE).read_text()),
            expected_root=releases,
        )
        assert release.stage_release(candidate, source, releases, git_sha) == manifest


def test_stage_accepts_a_superseding_dedup_manifest_and_refuses_a_malformed_one() -> None:
    """Schema 70 releases ship a schema-2 (superseding) dedup manifest; its identity is bound like v1."""
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="c" * 40)

        def manifest_json(payload: dict) -> str:
            payload = dict(payload)
            payload.pop("manifestSha256", None)
            payload["manifestSha256"] = hashlib.sha256(
                json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
            ).hexdigest()
            return json.dumps(payload)

        good = {
            "manifestSchema": 2,
            "supersedes": {"manifestSha256": "3" * 64},
            "algorithm": {"id": "cortex-cross-file-waveform-correlation-v2"},
            "summary": {"unconfirmedRiskGroups": 0},
        }
        path = base / "dedup-v2.json"
        path.write_text(manifest_json(good), encoding="utf-8")
        manifest = release.stage_release(candidate, source, releases, "c" * 40, path)
        assert manifest["dedupManifestSha256"] == json.loads(path.read_text(encoding="utf-8"))["manifestSha256"]

        for label, broken in (
            ("no supersedes", {**good, "supersedes": None}),
            ("v1 algorithm", {**good, "algorithm": {"id": "cortex-cross-file-waveform-correlation-v1"}}),
            ("schema 3", {**good, "manifestSchema": 3}),
            ("unresolved risk", {**good, "summary": {"unconfirmedRiskGroups": 1}}),
        ):
            bad = base / f"bad-{label.replace(' ', '-')}.json"
            bad.write_text(manifest_json(broken), encoding="utf-8")
            try:
                release.validate_dedup_manifest(bad)
            except release.ReleaseError:
                pass
            else:
                raise AssertionError(f"a dedup manifest with {label} was accepted")


def test_stage_refuses_mismatched_or_ambiguous_embedded_build_identity() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="b" * 40)
        try:
            release.stage_release(candidate, source, releases, "a" * 40)
        except release.ReleaseError as error:
            assert "was built from Git SHA" in str(error)
        else:
            raise AssertionError("stage accepted binaries built from a different Git commit")

        seed_candidate(candidate, git_sha="a" * 40)
        with (candidate / "pool_admin.exe").open("ab") as handle:
            handle.write(b"CORTEX_BUILD_SHA:" + ("a" * 40).encode("ascii"))
        try:
            release.stage_release(candidate, source, releases, "a" * 40)
        except release.ReleaseError as error:
            assert "exactly one" in str(error)
        else:
            raise AssertionError("stage accepted an executable with ambiguous build identity")


def test_streamed_build_identity_handles_block_boundary_without_false_marker() -> None:
    with tempfile.TemporaryDirectory() as raw:
        path = Path(raw) / "large.exe"
        sha = "c" * 40
        prefix = b"CORTEX_BUILD_SHA:"
        path.write_bytes(b"x" * (1024 * 1024 - 10) + prefix + sha.encode("ascii") + b"\0tail")
        assert release.validate_baked_git_sha(path, sha, "boundary fixture") == sha

        # The 41st lower-hex digit arrives in the next block. Chunk-local regex matching must not
        # mistake the first 40 digits for a valid marker before it has seen that disqualifier.
        path.write_bytes(
            b"x" * (1024 * 1024 - 10) + prefix + sha.encode("ascii") + b"d\0tail"
        )
        try:
            release.validate_baked_git_sha(path, sha, "boundary fixture")
        except release.ReleaseError as error:
            assert "exactly one" in str(error)
        else:
            raise AssertionError("streamed marker validation accepted a 41-hex build identity")


def test_operations_bundle_is_part_of_identity_and_tampering_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="d" * 40)
        first = release.stage_release(candidate, source, releases, "d" * 40)
        (source / "scripts" / "release_private_production.py").write_text("# changed controller\n", encoding="utf-8")
        second = release.stage_release(candidate, source, releases, "d" * 40)
        assert first["releaseId"] != second["releaseId"]
        staged_controller = Path(second["directory"]) / "scripts" / "release_private_production.py"
        staged_controller.write_text("# tampered after publication\n", encoding="utf-8")
        try:
            release.validate_manifest(second, expected_root=releases)
        except release.ReleaseError as error:
            assert "operations bundle" in str(error)
        else:
            raise AssertionError("changed recovery/controller bytes must invalidate the immutable release")


def test_operations_bundle_binds_runtime_dialect_authority() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="8" * 40)
        manifest = release.stage_release(candidate, source, releases, "8" * 40)
        staged_dialects = Path(manifest["directory"]) / "src-tauri" / "src" / "dialect.rs"
        staged_dialects.write_text("// tampered dialect authority\n", encoding="utf-8")
        try:
            release.validate_manifest(manifest, expected_root=releases)
        except release.ReleaseError as error:
            assert "operations bundle" in str(error)
        else:
            raise AssertionError("changed dialect authority must invalidate the immutable release")


def test_only_a_previous_release_may_use_the_legacy_operations_digest() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="7" * 40)
        manifest = release.stage_release(candidate, source, releases, "7" * 40)
        final = Path(manifest["directory"])
        (final / "src-tauri" / "src" / "dialect.rs").unlink()
        legacy_digest = release.operations_bundle_sha256(final, allow_legacy_missing_dialect=True)
        manifest["operationsSha256"] = legacy_digest

        assert release.validate_manifest(
            manifest,
            expected_root=releases,
            allow_compatible_previous=True,
        ) == manifest
        try:
            release.validate_manifest(manifest, expected_root=releases)
        except release.ReleaseError as error:
            assert "dialect authority" in str(error)
        else:
            raise AssertionError("a new candidate accepted the legacy operations digest")

        staged_controller = final / "scripts" / "release_private_production.py"
        staged_controller.write_text("# tampered legacy controller\n", encoding="utf-8")
        try:
            release.validate_manifest(
                manifest,
                expected_root=releases,
                allow_compatible_previous=True,
            )
        except release.ReleaseError as error:
            assert "operations bundle" in str(error)
        else:
            raise AssertionError("legacy compatibility accepted changed operational bytes")


def test_tampered_release_is_refused_and_never_replaced() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="b" * 40)
        manifest = release.stage_release(candidate, source, releases, "b" * 40)
        app = Path(manifest["appExe"])
        app.write_bytes(b"tampered")
        try:
            release.stage_release(candidate, source, releases, "b" * 40)
        except release.ReleaseError as error:
            assert "SHA-256" in str(error)
        else:
            raise AssertionError("an immutable release with changed bytes must fail closed")
        assert app.read_bytes() == b"tampered", "staging must never overwrite or hide a changed release"


def test_candidate_inside_live_release_root_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases = base / "source", base / "releases"
        candidate = releases / "candidate"
        seed_source(source)
        seed_candidate(candidate, git_sha="c" * 40)
        try:
            release.stage_release(candidate, source, releases, "c" * 40)
        except release.ReleaseError as error:
            assert "outside" in str(error)
        else:
            raise AssertionError("a build inside the live release root is not an isolated candidate")


def test_schema_rollback_policy_never_destroys_post_migration_work() -> None:
    assert release.rollback_policy(65, 70, 2, 2, 65) == "restore-pre-migration"
    assert release.rollback_policy(65, 65, 2, 2, 65) == "resume-pre-migration"
    assert release.rollback_policy(65, 65, 2, 3, 65) == "resume-pre-migration"
    assert release.rollback_policy(65, 70, 2, 3, 65) == "preserve-current"
    assert release.rollback_policy(65, 70, 2, 2, 65, database_changed=True) == "preserve-current"
    assert release.rollback_policy(70, 70, 20, 20, 70) == "binary-only"
    assert release.rollback_policy(70, 70, 20, 21, 70) == "binary-only"
    assert release.rollback_policy(70, 70, 20, 20, 65) == "blocked"
    assert release.rollback_policy(65, 71, 2, 2, 65) == "blocked"
    assert release.rollback_policy(64, 70, 2, 2, 64) == "blocked"
    # The schema-69 line (the release that served until the dedup-supersession release) is a proven
    # migration source: an interrupted 69->70 handover resumes or restores exactly like 65->70 did.
    assert release.rollback_policy(69, 69, 7, 7, 69) == "resume-pre-migration"
    assert release.rollback_policy(69, 70, 7, 7, 69) == "restore-pre-migration"
    assert release.rollback_policy(69, 70, 7, 8, 69) == "preserve-current"
    assert release.rollback_policy(69, 70, 7, 7, 69, database_changed=True) == "preserve-current"
    assert release.rollback_policy(68, 70, 2, 2, 68) == "blocked"


def test_only_exact_legacy_schema65_pointer_is_a_compatible_previous_boundary() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="e" * 40)
        current = release.stage_release(candidate, source, releases, "e" * 40)
        legacy = as_legacy_v65(current)
        assert release.validate_manifest(legacy, expected_root=releases, allow_compatible_previous=True) == legacy
        try:
            release.validate_manifest(legacy, expected_root=releases)
        except release.ReleaseError as error:
            assert "fields" in str(error)
        else:
            raise AssertionError("a legacy schema-65 release must never be accepted as a schema-70 candidate")
        for unsupported in (63, 64):
            changed = dict(legacy, expectedDatabaseSchema=unsupported)
            try:
                release.validate_manifest(changed, expected_root=releases, allow_compatible_previous=True)
            except release.ReleaseError as error:
                assert "database schema 65" in str(error)
            else:
                raise AssertionError(f"legacy schema {unsupported} must not be accepted as a migration boundary")


def as_previous_v69(manifest: dict[str, object], release_dir: Path) -> dict[str, object]:
    """Rewrite a staged release into the exact shape the live schema-69 pointer has.

    Its immutable directory carries the 1..69 migration catalog and the 65-to-69 contract whose
    digests bind that catalog, exactly as `stage_release` wrote them on 2026-09-02.
    """
    migrations = release_dir / "src-tauri" / "src" / "migrations" / "mod.rs"
    text = migrations.read_text(encoding="utf-8").replace("\r\n", "\n")
    cut = text.index("    Migration {\n        version: 70,")
    end = text.index("\n];", cut)
    text = text[:cut].rstrip("\n") + text[end:]
    migrations.write_text(text, encoding="utf-8", newline="\n")
    contract_path = release_dir / release.SCHEMA_CONTRACT_RELATIVE_PATH
    contract = json.loads(contract_path.read_text(encoding="utf-8"))
    contract["contractId"] = "cortex-private-production-schema-65-to-69-v1"
    contract["targetSchema"] = 69
    contract["supportedMigrationSources"] = [65]
    contract["migrationSourceSha256"] = hashlib.sha256(text.encode("utf-8")).hexdigest()
    contract_path.write_text(json.dumps(contract, indent=2) + "\n", encoding="utf-8", newline="\n")
    previous = dict(manifest)
    # The operations bundle digest covers scripts/ and the migration catalog, both rewritten above.
    previous["operationsSha256"] = release.operations_bundle_sha256(release_dir)
    previous["expectedDatabaseSchema"] = 69
    previous["schemaContractId"] = contract["contractId"]
    previous["schemaContractSha256"] = release.sha256_file(contract_path)
    return previous


def test_schema69_previous_pointer_is_compatible_but_never_a_candidate() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases = base / "source", base / "candidate", base / "releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="d" * 40)
        staged = release.stage_release(candidate, source, releases, "d" * 40)
        previous = as_previous_v69(staged, Path(str(staged["directory"])))
        accepted = release.validate_manifest(previous, expected_root=releases, allow_compatible_previous=True)
        assert accepted["expectedDatabaseSchema"] == 69
        for label, manifest in (
            ("candidate", previous),
            ("wrong contract", dict(previous, schemaContractId="cortex-private-production-schema-65-to-70-v1")),
        ):
            try:
                release.validate_manifest(
                    manifest, expected_root=releases, allow_compatible_previous=(label != "candidate")
                )
            except release.ReleaseError as error:
                assert "schema" in str(error), (label, error)
            else:
                raise AssertionError(f"a schema-69 pointer was accepted as a {label}")
        # An unproven source (schema 68) on a versioned pointer is refused even as a previous release.
        try:
            release.validate_manifest(
                dict(previous, expectedDatabaseSchema=68), expected_root=releases, allow_compatible_previous=True
            )
        except release.ReleaseError as error:
            assert "database schema 70" in str(error)
        else:
            raise AssertionError("schema 68 is not a proven migration source")


def test_checked_in_schema_contract_is_the_exact_65_to_70_authority() -> None:
    path, contract, digest = release.validate_schema_contract(APP / "scripts" / release.SCHEMA_CONTRACT_FILE)
    assert path.name == release.SCHEMA_CONTRACT_FILE
    assert contract["contractId"] == "cortex-private-production-schema-65-to-70-v1"
    assert contract["supportedMigrationSources"] == [65, 69]
    assert contract["targetSchema"] == 70
    assert contract["sameSchemaRecovery"] is True
    assert digest == release.sha256_file(path)

    with tempfile.TemporaryDirectory() as raw:
        source = Path(raw) / "source"
        seed_source(source)
        changed = json.loads((source / release.SCHEMA_CONTRACT_RELATIVE_PATH).read_text(encoding="utf-8"))
        changed["targetSchema"] = 71
        (source / release.SCHEMA_CONTRACT_RELATIVE_PATH).write_text(json.dumps(changed), encoding="utf-8")
        try:
            release.validate_schema_contract(source / release.SCHEMA_CONTRACT_RELATIVE_PATH)
        except release.ReleaseError as error:
            assert "exactly 70" in str(error)
        else:
            raise AssertionError("a rewritten target schema unexpectedly retained release authority")


def test_clone_preflight_proves_65_to_70_and_same_schema_70() -> None:
    for source_schema in (65, 69, 70):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            source, candidate_dir, releases, data = (
                base / "source",
                base / "candidate",
                base / "releases",
                base / "data",
            )
            seed_source(source)
            seed_candidate(candidate_dir, git_sha="f" * 40)
            manifest = release.stage_release(candidate_dir, source, releases, "f" * 40)
            data.mkdir()
            seed_database(data / "cortex-speech.db", source_schema)

            def fake_run_json(command: list[str], *, timeout: int = 300) -> dict[str, object]:
                verb = command[1]
                db = Path(command[command.index("--db") + 1])
                if verb == "migrate":
                    before = release.database_schema(db)
                    if before < 70:
                        connection = sqlite3.connect(db)
                        connection.executemany(
                            "INSERT INTO schema_migrations(version, description) VALUES(?, ?)",
                            [(item, f"migration-{item}") for item in range(before + 1, 71)],
                        )
                        connection.commit()
                        connection.close()
                    return {
                        "migrated": before != 70,
                        "beforeSchemaVersion": before,
                        "afterSchemaVersion": release.database_schema(db),
                        "appGitSha": manifest["appGitSha"],
                    }
                if verb in {"apply-dedup", "stamp-rights"}:
                    return {}
                if verb == "certify":
                    assert release.database_schema(db) == 70
                    return {
                        "appGitSha": manifest["appGitSha"],
                        "databaseSchemaVersion": 70,
                        "database": {"healthy": True},
                        "audio": {"allAvailable": True},
                        "rights": {"allExact": True},
                    }
                raise AssertionError(f"unexpected preflight command: {command}")

            with mock.patch.object(release, "run_json", side_effect=fake_run_json):
                proof = release.preflight_clone(data, manifest)
            assert proof["sourceSchemaVersion"] == source_schema
            assert proof["migration"]["migrated"] is (source_schema != 70)
            assert proof["certification"]["databaseSchemaVersion"] == 70


def test_clone_preflight_refuses_future_schema_before_candidate_execution() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate_dir, releases, data = (
            base / "source",
            base / "candidate",
            base / "releases",
            base / "data",
        )
        seed_source(source)
        seed_candidate(candidate_dir, git_sha="9" * 40)
        manifest = release.stage_release(candidate_dir, source, releases, "9" * 40)
        data.mkdir()
        seed_database(data / "cortex-speech.db", 71)
        with mock.patch.object(release, "run_json") as runner:
            try:
                release.preflight_clone(data, manifest)
            except release.ReleaseError as error:
                assert "not schema 71" in str(error)
            else:
                raise AssertionError("future schema unexpectedly entered candidate migration")
        runner.assert_not_called()


def test_database_schema_refuses_a_forged_max_version_with_gaps() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db = Path(raw) / "gap.db"
        connection = sqlite3.connect(db)
        connection.executescript(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT);"
            "INSERT INTO schema_migrations VALUES(1, 'one');"
            "INSERT INTO schema_migrations VALUES(70, 'forged max');"
        )
        connection.commit()
        connection.close()
        try:
            release.database_schema(db)
        except release.ReleaseError as error:
            assert "contiguous" in str(error)
        else:
            raise AssertionError("a gapped migration history unexpectedly became schema authority")


def test_database_content_authority_includes_committed_wal_frames() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db = Path(raw) / "wal.db"
        seed_database(db, 70, "before")
        writer = sqlite3.connect(db)
        try:
            assert writer.execute("PRAGMA journal_mode=WAL").fetchone()[0].lower() == "wal"
            writer.execute("PRAGMA wal_autocheckpoint=0")
            before = release.database_content_sha256(db)
            writer.execute("INSERT INTO marker VALUES('committed-only-in-wal')")
            writer.commit()
            wal = Path(str(db) + "-wal")
            assert wal.is_file() and wal.stat().st_size > 0
            after = release.database_content_sha256(db)
        finally:
            writer.close()
        assert after != before, "rollback authority ignored a committed WAL-visible write"


def test_stop_app_targets_one_exact_executable_and_waits_for_exit() -> None:
    if os.name != "nt":
        return
    ping = Path(os.environ.get("WINDIR", r"C:\Windows")) / "System32" / "ping.exe"
    if not ping.is_file():
        print(f"SKIP-ENV: {ping} missing (the stand-in process image this test stops)")
        return
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        targeted, bystander = (
            base / "active-release" / "cortex-speech-app.exe",
            base / "other-release" / "cortex-speech-app.exe",
        )
        processes: dict[Path, subprocess.Popen[bytes]] = {}
        for path, launch_as in (
            # The targeted copy is launched through the 8.3 form of its directory, which is what
            # Windows then reports as its image path (a hosted runner's shortened profile dir, a
            # shortcut through C:\PROGRA~1\...). stop_app is still handed the long path, so an
            # identity compare that does not normalise both sides matches nothing, stops nothing,
            # and still reports success.
            (targeted, short_directory_launch_path),
            (bystander, str),
        ):
            path.parent.mkdir()
            shutil.copy2(ping, path)
            # `ping -t` never exits on its own, so nothing below depends on how long the stand-in
            # runs: the only thing that can end either process is stop_app.
            processes[path] = subprocess.Popen(
                [launch_as(path), "127.0.0.1", "-t"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
        try:
            release.stop_app([targeted], force_after_seconds=1)
            # stop_app only returns once it has confirmed the kernel no longer lists the target, so
            # this reaps an already-dead handle rather than waiting on the stand-in's runtime.
            try:
                processes[targeted].wait(timeout=3)
            except subprocess.TimeoutExpired:
                raise AssertionError(
                    "stop_app reported success while the targeted executable was still running"
                ) from None
            assert (
                processes[bystander].poll() is None
            ), "stop_app stopped a same-named executable living at a different path"

            # Positive control: the bystander is a process stop_app can reach, so surviving above
            # was the exact-path filter doing its job and not this fixture being unkillable.
            release.stop_app([bystander], force_after_seconds=1)
            try:
                processes[bystander].wait(timeout=3)
            except subprocess.TimeoutExpired:
                raise AssertionError(
                    "stop_app could not stop the bystander even when it was the named target, so "
                    "its survival above proves nothing about exact targeting"
                ) from None
        finally:
            for process in processes.values():
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=3)


def test_restore_preserves_failed_database_and_verifies_snapshot() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, snapshot = base / "data", base / "snapshot"
        data.mkdir()
        snapshot.mkdir()
        for path, version, marker in (
            (data / "cortex-speech.db", 70, "failed-v70"),
            (snapshot / "cortex-speech.db", 65, "known-good-v65"),
        ):
            seed_database(path, version, marker)
        manifest_sha = seal_snapshot(snapshot)
        preserved = release.restore_database(snapshot, data, 65, manifest_sha)
        assert release.database_schema(data / "cortex-speech.db") == 65
        assert release.database_schema(preserved) == 70
        connection = sqlite3.connect(data / "cortex-speech.db")
        assert connection.execute("SELECT value FROM marker").fetchone()[0] == "known-good-v65"
        connection.close()


def test_handover_refuses_a_freshly_bound_but_stale_snapshot_generation() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, snapshot = base / "data", base / "snapshot"
        data.mkdir()
        snapshot.mkdir()
        seed_database(data / "cortex-speech.db", 65, "current-stopped-generation")
        seed_database(snapshot / "cortex-speech.db", 65, "older-self-consistent-generation")
        seal_snapshot(snapshot)
        result = subprocess.CompletedProcess(
            args=["create_recovery_snapshot.py"],
            returncode=0,
            stdout=f"LOCAL_SNAPSHOT={snapshot}\n",
            stderr="",
        )
        with mock.patch.object(release, "run", return_value=result):
            try:
                release.snapshot_before_handover(data, {"directory": str(base)})
            except release.ReleaseError as error:
                assert "exact stopped live database generation" in str(error)
            else:
                raise AssertionError("handover bound a valid manifest around an older database generation")


def test_restore_requires_the_exact_sealed_snapshot_bound_into_the_journal() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, expected_snapshot, other_snapshot = (
            base / "data",
            base / "expected-snapshot",
            base / "other-snapshot",
        )
        data.mkdir()
        expected_snapshot.mkdir()
        other_snapshot.mkdir()
        seed_database(data / "cortex-speech.db", 70, "live-must-survive")
        seed_database(expected_snapshot / "cortex-speech.db", 65, "journal-authority")
        seed_database(other_snapshot / "cortex-speech.db", 65, "older-but-self-consistent")
        expected_manifest_sha = seal_snapshot(expected_snapshot)
        other_manifest_sha = seal_snapshot(other_snapshot)
        assert other_manifest_sha != expected_manifest_sha

        try:
            release.restore_database(other_snapshot, data, 65, expected_manifest_sha)
        except release.ReleaseError as error:
            assert "exact snapshot captured" in str(error)
        else:
            raise AssertionError("rollback accepted a different self-consistent schema-65 snapshot")

        unsealed = base / "unsealed"
        unsealed.mkdir()
        seed_database(unsealed / "cortex-speech.db", 65, "manifest-missing")
        try:
            release.restore_database(unsealed, data, 65, expected_manifest_sha)
        except release.ReleaseError as error:
            assert "snapshot manifest" in str(error)
        else:
            raise AssertionError("rollback accepted a manifest-less database directory")

        connection = sqlite3.connect(data / "cortex-speech.db")
        try:
            assert connection.execute("SELECT value FROM marker").fetchone()[0] == "live-must-survive"
        finally:
            connection.close()
        assert not (data / "recovery-quarantine").exists()


def test_restore_refuses_a_real_concurrent_windows_instance_lock_holder() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, snapshot = base / "data", base / "snapshot"
        data.mkdir()
        snapshot.mkdir()
        seed_database(data / "cortex-speech.db", 70, "live-under-lock")
        seed_database(snapshot / "cortex-speech.db", 65, "rollback")
        manifest_sha = seal_snapshot(snapshot)
        ready = base / "holder.ready"
        holder = subprocess.Popen(
            [
                "powershell.exe",
                "-NoProfile",
                "-Command",
                (
                    "& { param($lockPath, $readyPath) "
                    "$stream = [System.IO.File]::Open($lockPath, "
                    "[System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, "
                    "[System.IO.FileShare]::None); "
                    "try { [System.IO.File]::WriteAllText($readyPath, 'ready'); Start-Sleep -Seconds 120 } "
                    "finally { $stream.Dispose() } }"
                ),
                str(data / "cortex.lock"),
                str(ready),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            # Wait on the REAL condition -- the holder publishing its readiness file -- not on a
            # guessed duration. The old 5s bound was a cold-PowerShell-startup guess that held on a
            # quiet workstation and failed on windows-latest (measured 2026-09-01: "disposable lock
            # holder did not start"). The holder is killed in the finally below and now sleeps well
            # past this budget, so a generous bound costs nothing; the loop still exits immediately
            # if the holder dies, so a genuine failure is reported at once rather than after 60s.
            deadline = time.monotonic() + 60
            while not ready.is_file() and holder.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            if holder.poll() is not None:
                raise AssertionError(
                    "disposable lock holder exited before publishing readiness "
                    f"(returncode {holder.returncode}); it never held the lock, so the refusal this "
                    "test asserts would be vacuous"
                )
            assert ready.is_file(), (
                "disposable lock holder did not publish readiness within 60s though it is still "
                "running; treat this as a slow start, not a crash"
            )
            try:
                release.restore_database(snapshot, data, 65, manifest_sha)
            except release.ReleaseError as error:
                assert "another Cortex writer holds" in str(error)
            else:
                raise AssertionError("rollback replaced the database while a real writer lock was held")
        finally:
            if holder.poll() is None:
                holder.terminate()
            holder.wait(timeout=5)
        connection = sqlite3.connect(data / "cortex-speech.db")
        try:
            assert connection.execute("SELECT value FROM marker").fetchone()[0] == "live-under-lock"
        finally:
            connection.close()


def test_interrupted_65_to_70_handover_restores_schema65_and_reactivates_legacy_release() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases, data, snapshot = (
            base / "source",
            base / "releases",
            base / "data",
            base / "snapshot",
        )
        seed_source(source)
        candidate_dir, previous_dir = base / "candidate", base / "previous"
        seed_candidate(candidate_dir, b"candidate-v70", b"candidate-admin-v70", "a" * 40)
        seed_candidate(previous_dir, b"previous-v65", b"previous-admin-v65", "b" * 40)
        candidate = release.stage_release(candidate_dir, source, releases, "a" * 40)
        previous = as_legacy_v65(release.stage_release(previous_dir, source, releases, "b" * 40))
        data.mkdir()
        snapshot.mkdir()
        for path, version, marker in (
            (data / "cortex-speech.db", 70, "failed-v70"),
            (snapshot / "cortex-speech.db", 65, "known-good-v65"),
        ):
            seed_database(path, version, marker)
        seal_snapshot(snapshot)
        release.atomic_json(
            data / release.JOURNAL_FILE,
            release_journal(
                candidate,
                previous,
                source_schema=65,
                phase="snapshotted",
                snapshot=snapshot,
                target_digest=None,
            ),
        )
        calls: list[tuple[str, object]] = []
        with (
            mock.patch.object(release, "task_change"),
            mock.patch.object(release, "stop_app"),
            mock.patch.object(release, "launch_app", side_effect=lambda path: calls.append(("launch", path))),
            mock.patch.object(release, "wait_for_server"),
            mock.patch.object(
                release, "certify_live", side_effect=lambda _data, manifest: calls.append(("certify", manifest))
            ),
            mock.patch.object(release, "prove_links"),
            mock.patch.object(release, "prove_canonical_queues"),
            mock.patch.object(release, "register_release_tasks"),
            mock.patch.object(release, "unregister_task"),
        ):
            assert release.recover(data, releases)
        assert release.database_schema(data / "cortex-speech.db") == 65
        active = json.loads((data / release.POINTER_FILE).read_text(encoding="utf-8"))
        assert active["releaseId"] == previous["releaseId"]
        assert calls == [("launch", Path(previous["appExe"])), ("certify", previous)]
        assert not (data / release.MAINTENANCE_FILE).exists()
        assert not (data / release.JOURNAL_FILE).exists()


def test_pre_migration_recovery_preserves_a_decision_committed_during_stop_admission() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases, data = base / "source", base / "releases", base / "data"
        seed_source(source)
        candidate_dir, previous_dir = base / "candidate", base / "previous"
        seed_candidate(candidate_dir, git_sha="8" * 40)
        seed_candidate(previous_dir, git_sha="9" * 40)
        candidate = release.stage_release(candidate_dir, source, releases, "8" * 40)
        previous = as_legacy_v65(release.stage_release(previous_dir, source, releases, "9" * 40))
        data.mkdir()
        seed_database(data / "cortex-speech.db", 65, "pre-migration-live")
        connection = sqlite3.connect(data / "cortex-speech.db")
        connection.execute("INSERT INTO review_pool_decisions(id) VALUES(3)")
        connection.commit()
        connection.close()
        release.atomic_json(
            data / release.JOURNAL_FILE,
            release_journal(
                candidate,
                previous,
                source_schema=65,
                phase="maintenance",
                snapshot=None,
                target_digest=None,
                baseline=2,
            ),
        )

        with (
            mock.patch.object(release, "task_change"),
            mock.patch.object(release, "stop_app"),
            mock.patch.object(release, "restore_database") as restore,
            mock.patch.object(release, "launch_app"),
            mock.patch.object(release, "wait_for_server"),
            mock.patch.object(release, "certify_live"),
            mock.patch.object(release, "prove_links"),
            mock.patch.object(release, "prove_canonical_queues"),
            mock.patch.object(release, "register_release_tasks"),
            mock.patch.object(release, "unregister_task"),
        ):
            assert release.recover(data, releases)
        restore.assert_not_called()
        connection = sqlite3.connect(data / "cortex-speech.db")
        try:
            assert connection.execute("SELECT id FROM review_pool_decisions").fetchall() == [(3,)]
        finally:
            connection.close()
        assert json.loads((data / release.POINTER_FILE).read_text(encoding="utf-8"))["releaseId"] == previous[
            "releaseId"
        ]


def test_same_schema_70_recovery_uses_previous_binary_without_database_rollback() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases, data = base / "source", base / "releases", base / "data"
        seed_source(source)
        candidate_dir, previous_dir = base / "candidate", base / "previous"
        seed_candidate(candidate_dir, b"candidate-v70", b"candidate-admin-v70", "1" * 40)
        seed_candidate(previous_dir, b"previous-v70", b"previous-admin-v70", "2" * 40)
        candidate = release.stage_release(candidate_dir, source, releases, "1" * 40)
        previous = release.stage_release(previous_dir, source, releases, "2" * 40)
        data.mkdir()
        seed_database(data / "cortex-speech.db", 70, "same-schema")
        digest = release.database_content_sha256(data / "cortex-speech.db")
        release.atomic_json(
            data / release.JOURNAL_FILE,
            release_journal(
                candidate,
                previous,
                source_schema=70,
                phase="candidate-active",
                snapshot=None,
                target_digest=digest,
            ),
        )
        calls: list[tuple[str, object]] = []
        with (
            mock.patch.object(release, "task_change"),
            mock.patch.object(release, "stop_app"),
            mock.patch.object(release, "restore_database") as restore,
            mock.patch.object(release, "launch_app", side_effect=lambda path: calls.append(("launch", path))),
            mock.patch.object(release, "wait_for_server"),
            mock.patch.object(release, "certify_live"),
            mock.patch.object(release, "prove_links"),
            mock.patch.object(release, "prove_canonical_queues"),
            mock.patch.object(release, "register_release_tasks"),
            mock.patch.object(release, "unregister_task"),
        ):
            assert release.recover(data, releases)
        restore.assert_not_called()
        assert release.database_schema(data / "cortex-speech.db") == 70
        assert json.loads((data / release.POINTER_FILE).read_text(encoding="utf-8"))["releaseId"] == previous[
            "releaseId"
        ]
        assert calls == [("launch", Path(previous["appExe"]))]


def test_post_migration_database_write_refuses_rollback_to_schema65_snapshot() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases, data, snapshot = (
            base / "source",
            base / "releases",
            base / "data",
            base / "snapshot",
        )
        seed_source(source)
        candidate_dir, previous_dir = base / "candidate", base / "previous"
        seed_candidate(candidate_dir, b"candidate-v70", b"candidate-admin-v70", "3" * 40)
        seed_candidate(previous_dir, b"previous-v65", b"previous-admin-v65", "4" * 40)
        candidate = release.stage_release(candidate_dir, source, releases, "3" * 40)
        previous = as_legacy_v65(release.stage_release(previous_dir, source, releases, "4" * 40))
        data.mkdir()
        snapshot.mkdir()
        seed_database(data / "cortex-speech.db", 70, "certified-before-exposure")
        seed_database(snapshot / "cortex-speech.db", 65, "pre-migration")
        seal_snapshot(snapshot)
        certified_digest = release.database_content_sha256(data / "cortex-speech.db")
        release.atomic_json(
            data / release.JOURNAL_FILE,
            release_journal(
                candidate,
                previous,
                source_schema=65,
                phase="exposed",
                snapshot=snapshot,
                target_digest=certified_digest,
            ),
        )
        # This is deliberately not a pool decision: rollback safety must cover every committed DB
        # write, including owner-only state that the old max-pool-decision heuristic could not see.
        connection = sqlite3.connect(data / "cortex-speech.db")
        connection.execute("INSERT INTO marker VALUES('owner-write-after-migration')")
        connection.commit()
        connection.close()

        with (
            mock.patch.object(release, "task_change"),
            mock.patch.object(release, "stop_app"),
            mock.patch.object(release, "restore_database") as restore,
            mock.patch.object(release, "launch_app"),
            mock.patch.object(release, "wait_for_server"),
            mock.patch.object(release, "certify_live"),
            mock.patch.object(release, "prove_links"),
            mock.patch.object(release, "prove_canonical_queues"),
            mock.patch.object(release, "register_release_tasks"),
            mock.patch.object(release, "unregister_task"),
        ):
            assert release.recover(data, releases)
        restore.assert_not_called()
        assert release.database_schema(data / "cortex-speech.db") == 70
        connection = sqlite3.connect(data / "cortex-speech.db")
        retained = connection.execute(
            "SELECT COUNT(*) FROM marker WHERE value='owner-write-after-migration'"
        ).fetchone()[0]
        assert retained == 1
        connection.close()
        pointer = json.loads((data / release.POINTER_FILE).read_text(encoding="utf-8"))
        assert pointer["releaseId"] == candidate["releaseId"]


def test_recovery_refuses_future_schema_before_process_or_task_mutation() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, releases, data = base / "source", base / "releases", base / "data"
        seed_source(source)
        candidate_dir, previous_dir = base / "candidate", base / "previous"
        seed_candidate(candidate_dir, b"candidate-v70", b"candidate-admin-v70", "5" * 40)
        seed_candidate(previous_dir, b"previous-v70", b"previous-admin-v70", "6" * 40)
        candidate = release.stage_release(candidate_dir, source, releases, "5" * 40)
        previous = release.stage_release(previous_dir, source, releases, "6" * 40)
        data.mkdir()
        seed_database(data / "cortex-speech.db", 71, "future")
        release.atomic_json(
            data / release.JOURNAL_FILE,
            release_journal(
                candidate,
                previous,
                source_schema=70,
                phase="prepared",
                snapshot=None,
                target_digest=None,
            ),
        )
        with (
            mock.patch.object(release, "task_change") as task_change,
            mock.patch.object(release, "stop_app") as stop_app,
        ):
            try:
                release.recover(data, releases)
            except release.ReleaseError as error:
                assert "future database schema 71" in str(error)
            else:
                raise AssertionError("future-schema recovery unexpectedly continued")
        task_change.assert_not_called()
        stop_app.assert_not_called()
        assert not (data / release.MAINTENANCE_FILE).exists()


def test_watchdog_and_server_pin_the_release_boundary() -> None:
    watchdog = (APP / "scripts" / "ops" / "cortex-watchdog.ps1").read_text(encoding="utf-8")
    supervision = (APP / "scripts" / "check_supervision_live.py").read_text(encoding="utf-8")
    couch = couch_surface(APP / "src-tauri" / "src")
    controller = SUBJECT.read_text(encoding="utf-8")
    assert release.POINTER_FILE in watchdog
    assert "Get-VerifiedActiveRelease" in watchdog
    assert "function Get-Sha256Hex" in watchdog
    assert "function Get-Sha256Utf8Lf" in watchdog
    assert "$actualSha = Get-Sha256Hex $check[0]" in watchdog
    assert "(Get-FileHash" not in watchdog
    assert release.SCHEMA_CONTRACT_FILE in watchdog
    assert "cortex-private-production-schema-65-to-70-v1" in watchdog
    assert "release pointer does not require private-production database schema 70" in watchdog
    assert "$dedup.manifestSchema -notin @(1, 2)" in watchdog, "the watchdog must accept a superseding (schema-2) dedup manifest"
    assert "-or $sources[1] -isnot [int] -or $sources[1] -ne 69 `" in watchdog
    assert "legacy release pointer is not the exact schema-65 handover boundary" in watchdog
    assert "Test-ActiveReleaseDatabaseSchema $poolAdmin $dbPath" in watchdog
    assert "$process.WaitForExit(60000)" in watchdog
    assert "blocked (active release database schema mismatch)" in watchdog
    assert "validate_manifest(value, expected_root=release_root, allow_compatible_previous=True)" in supervision
    assert release.WATCHDOG_TASK == "CortexPrivateProductionWatchdog"
    assert release.LEGACY_WATCHDOG_TASK == "CortexWatchdog"
    assert '"-TaskName",\n        WATCHDOG_TASK' in controller
    assert "Wait-Process -Id $left.Id -Timeout 10" in controller
    assert "Cortex app process did not stop after the force deadline" in controller
    assert "New-ScheduledTaskTrigger -AtLogOn -User $currentPrincipal" in watchdog
    assert "$clock = New-ScheduledTaskTrigger -Once" in watchdog
    assert "-Trigger @($logon, $clock)" in watchdog
    assert release.MAINTENANCE_FILE in couch
    probe = couch.index('if path == "/api/claim/probe"')
    maintenance = couch.index("if maintenance", probe)
    auth = couch.index("let authenticated", maintenance)
    assert probe < maintenance < auth, "only the non-mutating link probe may precede maintenance refusal"


def test_supervision_accepts_exact_schema_v2_pointer_and_rejects_contract_drift() -> None:
    with tempfile.TemporaryDirectory() as raw:
        # resolve(): _private_watchdog_problem resolves watchdogScript (strict) and requires that
        # canonical spelling inside the scheduled action; macOS temp is aliased (/var -> /private/var).
        base = Path(raw).resolve()
        source, candidate, data = base / "source", base / "candidate", base / "data"
        localappdata = base / "local"
        releases = localappdata / "CortexSpeech" / "private-production-releases"
        seed_source(source)
        seed_candidate(candidate, git_sha="7" * 40)
        manifest = release.stage_release(candidate, source, releases, "7" * 40)
        data.mkdir()
        release.atomic_json(data / release.POINTER_FILE, manifest)
        watchdog = str(Path(manifest["watchdogScript"]))
        with (
            mock.patch.dict(os.environ, {"LOCALAPPDATA": str(localappdata), "APPDATA": str(base / "appdata")}),
            mock.patch.object(supervision, "_watchdog_action_arguments", return_value=f'-File "{watchdog}"'),
        ):
            assert supervision._private_watchdog_problem(data) is None
            drifted = dict(manifest, schemaContractSha256="0" * 64)
            release.atomic_json(data / release.POINTER_FILE, drifted)
            problem = supervision._private_watchdog_problem(data)
        assert problem is not None and "schema contract" in problem


def test_watchdog_refuses_a_malformed_active_pointer_before_probing_or_launching() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        data = Path(raw)
        (data / release.POINTER_FILE).write_text('{"schema":1}\n', encoding="utf-8")
        env = dict(os.environ, CORTEX_WATCHDOG_DATA_DIR=str(data), CORTEX_WATCHDOG_PORT="1")
        result = subprocess.run(
            [
                "powershell.exe",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(APP / "scripts" / "ops" / "cortex-watchdog.ps1"),
                "-DryRun",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout


def run_watchdog_with_pointer(
    data: Path,
    *,
    port: int = 1,
    exe: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ, CORTEX_WATCHDOG_DATA_DIR=str(data), CORTEX_WATCHDOG_PORT=str(port))
    if exe is not None:
        env["CORTEX_WATCHDOG_EXE"] = str(exe)
    return subprocess.run(
        [
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(APP / "scripts" / "ops" / "cortex-watchdog.ps1"),
            "-DryRun",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def test_watchdog_binds_a_live_responder_to_the_exact_supervised_process() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        data = Path(raw)
        server = ThreadingHTTPServer(("127.0.0.1", 0), QuietHealthHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            port = int(server.server_address[1])
            unrelated = data / "not-running" / "cortex-speech-app.exe"
            result = run_watchdog_with_pointer(data, port=port, exe=unrelated)
            assert result.returncode != 0
            assert (
                "WATCHDOG-ACTION: blocked (responding port is not owned by active release)"
                in result.stdout
            )

            # Positive control: this Python process owns the disposable listener, so pointing the
            # supervised path at its exact image must make the same real PowerShell probe report alive.
            result = run_watchdog_with_pointer(data, port=port, exe=current_windows_process_image())
            assert result.returncode == 0, result.stdout + result.stderr
            assert "WATCHDOG-ACTION: alive" in result.stdout
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


def test_watchdog_accepts_a_schema2_dedup_manifest_pointer_and_refuses_schema3() -> None:
    """2026-09-06 live handover rolled back: the watchdog pinned manifestSchema == 1 and refused the
    schema-2 (superseding) manifest during task re-registration. Run the real watchdog on both."""
    if os.name != "nt":
        return

    def dedup_json(schema: int) -> str:
        payload = {
            "manifestSchema": schema,
            "supersedes": {"manifestSha256": "3" * 64},
            "algorithm": {"id": "cortex-cross-file-waveform-correlation-v2"},
            "summary": {"unconfirmedRiskGroups": 0},
        }
        payload["manifestSha256"] = hashlib.sha256(
            json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        return json.dumps(payload)

    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases, data = base / "source", base / "candidate", base / "releases", base / "data"
        seed_source(source)
        seed_candidate(candidate, git_sha="a" * 40)
        dedup_v2 = base / "dedup-v2.json"
        dedup_v2.write_text(dedup_json(2), encoding="utf-8")
        manifest = release.stage_release(candidate, source, releases, "a" * 40, dedup_v2)
        data.mkdir()
        release.atomic_json(data / release.POINTER_FILE, manifest)
        # No database in the data dir: a pointer that passes validation is blocked one step LATER, on the
        # missing database, never on its dedup identity.
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release database missing)" in result.stdout, result.stdout
        log_path = data / "logs" / "watchdog.log"
        assert "dedup manifest identity" not in (log_path.read_text(encoding="utf-8") if log_path.exists() else "")

        # The same pointer over a schema-3 manifest (digest self-consistent) is refused as invalid.
        staged_dedup = Path(str(manifest["dedupManifest"]))
        staged_dedup.write_text(dedup_json(3), encoding="utf-8")
        forged = dict(manifest, dedupManifestSha256=json.loads(staged_dedup.read_text(encoding="utf-8"))["manifestSha256"])
        release.atomic_json(data / release.POINTER_FILE, forged)
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout, result.stdout


def test_watchdog_recomputes_operations_content_instead_of_trusting_pointer_text() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases, data = (
            base / "source",
            base / "candidate",
            base / "releases",
            base / "data",
        )
        seed_source(source)
        seed_candidate(candidate, git_sha="a" * 40)
        manifest = release.stage_release(candidate, source, releases, "a" * 40)
        data.mkdir()
        release.atomic_json(data / release.POINTER_FILE, manifest)
        staged_controller = Path(str(manifest["directory"])) / "scripts" / "release_private_production.py"
        staged_controller.write_text("# changed after manifest publication\n", encoding="utf-8")
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout
        log = (data / "logs" / "watchdog.log").read_text(encoding="utf-8")
        assert "operations bundle does not match" in log


def test_watchdog_rejects_self_consistent_artifact_hash_with_wrong_embedded_sha() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        source, candidate, releases, data = (
            base / "source",
            base / "candidate",
            base / "releases",
            base / "data",
        )
        seed_source(source)
        seed_candidate(candidate, git_sha="a" * 40)
        manifest = release.stage_release(candidate, source, releases, "a" * 40)
        app = Path(str(manifest["appExe"]))
        app.write_bytes(
            app.read_bytes().replace(
                b"CORTEX_BUILD_SHA:" + ("a" * 40).encode("ascii"),
                b"CORTEX_BUILD_SHA:" + ("b" * 40).encode("ascii"),
            )
        )
        pointer = dict(manifest, appSha256=release.sha256_file(app))
        data.mkdir()
        release.atomic_json(data / release.POINTER_FILE, pointer)
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout
        log = (data / "logs" / "watchdog.log").read_text(encoding="utf-8")
        assert "build SHA does not match" in log


def test_watchdog_refuses_legacy_schema63_and_64_pointers() -> None:
    if os.name != "nt":
        return
    for unsupported in (63, 64):
        with tempfile.TemporaryDirectory() as raw:
            data = Path(raw)
            pointer = {key: None for key in release.LEGACY_V1_MANIFEST_FIELDS}
            pointer.update(schema=1, expectedDatabaseSchema=unsupported)
            (data / release.POINTER_FILE).write_text(json.dumps(pointer), encoding="utf-8")
            result = run_watchdog_with_pointer(data)
            assert result.returncode != 0
            assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout


def test_watchdog_refuses_contract_drift_and_schema_mismatch_before_process_control() -> None:
    if os.name != "nt":
        return
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw)
        data, immutable = base / "data", base / "immutable"
        data.mkdir()
        (immutable / "scripts" / "ops").mkdir(parents=True)
        (immutable / "src-tauri" / "src" / "migrations").mkdir(parents=True)
        shutil.copy2(
            APP / "scripts" / "ops" / "cortex-watchdog.ps1",
            immutable / "scripts" / "ops" / "cortex-watchdog.ps1",
        )
        shutil.copy2(
            APP / "scripts" / release.SCHEMA_CONTRACT_FILE,
            immutable / "scripts" / release.SCHEMA_CONTRACT_FILE,
        )
        shutil.copy2(
            APP / "scripts" / "append_only_migration_contract.v1.json",
            immutable / "scripts" / "append_only_migration_contract.v1.json",
        )
        shutil.copy2(
            APP / "src-tauri" / "src" / "migrations" / "mod.rs",
            immutable / "src-tauri" / "src" / "migrations" / "mod.rs",
        )
        shutil.copy2(
            APP / "src-tauri" / "src" / "dialect.rs",
            immutable / "src-tauri" / "src" / "dialect.rs",
        )
        build_marker = b"\0CORTEX_BUILD_SHA:" + ("a" * 40).encode("ascii") + b"\0"
        (immutable / "cortex-speech-app.exe").write_bytes(b"app" + build_marker)
        (immutable / "pool_admin.exe").write_bytes(b"admin-must-not-run" + build_marker)
        dedup = {"manifestSchema": 1, "summary": {"unconfirmedRiskGroups": 0}}
        dedup["manifestSha256"] = hashlib.sha256(
            json.dumps(dedup, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        (immutable / release.DEDUP_MANIFEST_FILE).write_text(json.dumps(dedup), encoding="utf-8")
        pointer = {
            "schema": 2,
            "releaseId": "test",
            "expectedDatabaseSchema": 70,
            "appGitSha": "a" * 40,
            "createdAtUtc": release.utc_now(),
            "directory": str(immutable),
            "appExe": str(immutable / "cortex-speech-app.exe"),
            "poolAdminExe": str(immutable / "pool_admin.exe"),
            "appSha256": release.sha256_file(immutable / "cortex-speech-app.exe"),
            "poolAdminSha256": release.sha256_file(immutable / "pool_admin.exe"),
            "watchdogScript": str(immutable / "scripts" / "ops" / "cortex-watchdog.ps1"),
            "watchdogSha256": release.sha256_file(immutable / "scripts" / "ops" / "cortex-watchdog.ps1"),
            "operationsSha256": release.operations_bundle_sha256(immutable),
            "schemaContract": str(immutable / "scripts" / release.SCHEMA_CONTRACT_FILE),
            "schemaContractId": release.SCHEMA_CONTRACT_ID,
            "schemaContractSha256": "0" * 64,
            "dedupManifest": str(immutable / release.DEDUP_MANIFEST_FILE),
            "dedupManifestSha256": dedup["manifestSha256"],
        }
        (data / release.POINTER_FILE).write_text(json.dumps(pointer), encoding="utf-8")
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release pointer invalid)" in result.stdout
        assert not (data / "cortex-speech.db").exists(), "contract refusal must happen before database access"

        pointer["schemaContractSha256"] = release.sha256_file(
            immutable / "scripts" / release.SCHEMA_CONTRACT_FILE
        )
        (data / release.POINTER_FILE).write_text(json.dumps(pointer), encoding="utf-8")
        (data / "cortex-speech.db").write_bytes(b"not-a-schema-70-database")
        result = run_watchdog_with_pointer(data)
        assert result.returncode != 0
        assert "WATCHDOG-ACTION: blocked (active release database schema mismatch)" in result.stdout


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PRIVATE PRODUCTION RELEASE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
