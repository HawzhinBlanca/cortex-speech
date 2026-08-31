#!/usr/bin/env python3
"""Pins verify-10's active and staged private-production release authority."""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from unittest import mock


APP = Path(__file__).resolve().parent.parent
VERIFY = APP.parent / "scripts" / "verify_10.py"
RELEASE = APP / "scripts" / "release_private_production.py"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


GATE = load_module("verify10_active_release_runtime", VERIFY)
RELEASE_GATE = load_module("verify10_release_fixture", RELEASE)
GIT_SHA = "a" * 40


def seed_source(root: Path) -> None:
    scripts = root / "scripts"
    (scripts / "ops").mkdir(parents=True)
    (scripts / "ops" / "cortex-watchdog.ps1").write_text(
        "Write-Output 'watchdog'\n", encoding="utf-8"
    )
    (scripts / "release_private_production.py").write_text("# controller\n", encoding="utf-8")
    shutil.copy2(
        APP / "scripts" / RELEASE_GATE.SCHEMA_CONTRACT_FILE,
        scripts / RELEASE_GATE.SCHEMA_CONTRACT_FILE,
    )
    shutil.copy2(
        APP / "scripts" / "append_only_migration_contract.v1.json",
        scripts / "append_only_migration_contract.v1.json",
    )
    migrations = root / "src-tauri" / "src" / "migrations"
    migrations.mkdir(parents=True)
    shutil.copy2(APP / "src-tauri" / "src" / "migrations" / "mod.rs", migrations / "mod.rs")
    dedup: dict[str, object] = {
        "manifestSchema": 1,
        "summary": {"unconfirmedRiskGroups": 0},
    }
    dedup["manifestSha256"] = hashlib.sha256(
        json.dumps(dedup, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    (root / RELEASE_GATE.DEDUP_MANIFEST_FILE).write_text(
        json.dumps(dedup), encoding="utf-8"
    )


def fixture() -> tuple[
    tempfile.TemporaryDirectory[str],
    Path,
    Path,
    dict[str, object],
    Path,
]:
    temporary = tempfile.TemporaryDirectory()
    # resolve(): macOS temp is /var/... -> /private/var/...; the staged-manifest guard requires
    # the canonical alias-free spelling, which is the fixture's job to supply.
    base = Path(temporary.name).resolve()
    source = base / "source"
    candidate = base / "candidate"
    appdata = base / "appdata"
    localappdata = base / "localappdata"
    root = localappdata / "CortexSpeech" / "private-production-releases"
    seed_source(source)
    candidate.mkdir()
    (candidate / "cortex-speech-app.exe").write_bytes(
        b"prefix CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii") + b" suffix"
    )
    (candidate / "pool_admin.exe").write_bytes(
        b"pool-admin CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii")
    )
    manifest = RELEASE_GATE.stage_release(candidate, source, root, GIT_SHA)
    return temporary, appdata, root, manifest, Path(str(manifest["appExe"]))


def mismatched_stage_fixture() -> tuple[
    tempfile.TemporaryDirectory[str],
    Path,
    dict[str, object],
]:
    """Emulate a legacy schema-v2 stage output that trusted appGitSha over the PE marker.

    Start with the real stage layout, then rebuild every path/hash/release-id field affected by a
    mismatched app binary. The resulting manifest is internally hash-consistent and differs only in
    the security property under test; the verifier must measure the marker instead of trusting the
    producer's appGitSha claim.
    """

    temporary, _appdata, root, manifest, exe = fixture()
    exe.write_bytes(
        b"prefix CORTEX_BUILD_SHA:" + ("b" * 40).encode("ascii") + b" suffix"
    )
    app_sha = hashlib.sha256(exe.read_bytes()).hexdigest()
    release_id = (
        f"{GIT_SHA[:12]}-{app_sha[:12]}-{str(manifest['operationsSha256'])[:12]}-"
        f"{str(manifest['schemaContractSha256'])[:12]}-"
        f"{str(manifest['dedupManifestSha256'])[:12]}"
    )
    old_directory = Path(str(manifest["directory"]))
    directory = root / release_id
    old_directory.rename(directory)
    changed = {
        **manifest,
        "releaseId": release_id,
        "directory": str(directory),
        "appExe": str(directory / "cortex-speech-app.exe"),
        "poolAdminExe": str(directory / "pool_admin.exe"),
        "appSha256": app_sha,
        "watchdogScript": str(directory / "scripts" / "ops" / "cortex-watchdog.ps1"),
        "dedupManifest": str(directory / RELEASE_GATE.DEDUP_MANIFEST_FILE),
        "schemaContract": str(directory / RELEASE_GATE.SCHEMA_CONTRACT_RELATIVE_PATH),
    }
    (directory / RELEASE_GATE.RELEASE_MANIFEST_FILE).write_text(
        json.dumps(changed), encoding="utf-8"
    )
    return temporary, root, changed


def test_valid_hash_bound_schema69_active_release_is_selected() -> None:
    temporary, _appdata, root, manifest, exe = fixture()
    try:
        assert GATE.validate_active_release_runtime(
            manifest, root, expected_sha=GIT_SHA
        ) == exe.resolve()
    finally:
        temporary.cleanup()


def test_release_manifest_boolean_numeric_authority_fails_closed() -> None:
    temporary, _appdata, root, manifest, _exe = fixture()
    try:
        for field in ("schema", "expectedDatabaseSchema"):
            changed = dict(manifest)
            changed[field] = True
            try:
                GATE.validate_active_release_runtime(changed, root, expected_sha=GIT_SHA)
            except ValueError:
                pass
            else:
                raise AssertionError(f"boolean {field} impersonated an integer release authority")
    finally:
        temporary.cleanup()


def test_active_release_hash_drift_fails_closed() -> None:
    temporary, _appdata, root, manifest, exe = fixture()
    try:
        exe.write_bytes(exe.read_bytes() + b"tampered")
        try:
            GATE.validate_active_release_runtime(manifest, root, expected_sha=GIT_SHA)
        except ValueError as error:
            assert "hash" in str(error).lower() or "sha-256" in str(error).lower()
        else:
            raise AssertionError("tampered immutable release was accepted")
    finally:
        temporary.cleanup()


def test_active_release_path_escape_fails_closed() -> None:
    temporary, _appdata, root, manifest, _exe = fixture()
    try:
        outside = Path(temporary.name) / "outside"
        outside.mkdir()
        changed = dict(manifest, directory=str(outside))
        try:
            GATE.validate_active_release_runtime(changed, root, expected_sha=GIT_SHA)
        except ValueError as error:
            assert "root" in str(error).lower() or "directory" in str(error).lower()
        else:
            raise AssertionError("release path outside the immutable root was accepted")
    finally:
        temporary.cleanup()


def test_active_release_manifest_git_sha_must_match_the_binary_marker() -> None:
    temporary, _appdata, root, manifest, _exe = fixture()
    try:
        changed = dict(manifest, appGitSha="b" * 40)
        try:
            GATE.validate_active_release_runtime(changed, root, expected_sha="b" * 40)
        except ValueError as error:
            assert "git sha" in str(error).lower() or "release id" in str(error).lower()
        else:
            raise AssertionError("manifest/binary Git mismatch was accepted")
    finally:
        temporary.cleanup()


def test_binary_identity_refuses_duplicate_or_noncanonical_build_markers() -> None:
    with tempfile.TemporaryDirectory() as raw:
        path = Path(raw) / "candidate.exe"
        exact = b"CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii")
        path.write_bytes(exact + b"\x00" + exact)
        _digest, _size, marker = GATE._binary_identity(path)
        assert marker is None
        path.write_bytes(b"CORTEX_BUILD_SHA:" + GIT_SHA.upper().encode("ascii"))
        _digest, _size, marker = GATE._binary_identity(path)
        assert marker == GIT_SHA.upper() and marker != GIT_SHA


def test_staged_candidate_does_not_trust_stage_output_with_mismatched_embedded_sha() -> None:
    temporary, root, manifest = mismatched_stage_fixture()
    try:
        manifest_path = Path(str(manifest["directory"])) / RELEASE_GATE.RELEASE_MANIFEST_FILE
        try:
            GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha=GIT_SHA, release_root=root
            )
        except GATE.EvidenceError as error:
            message = str(error).lower()
            assert "git sha" in message and (
                "embed" in message or "built from" in message
            )
        else:
            raise AssertionError(
                "verifier trusted a staged manifest whose executable embeds another Git SHA"
            )
    finally:
        temporary.cleanup()


def test_staged_candidate_binds_every_release_authority_and_is_noncertifying() -> None:
    temporary, appdata, root, manifest, exe = fixture()
    old_environment = {
        key: os.environ.get(key) for key in ("APPDATA", "LOCALAPPDATA", "CORTEX_APP_EXE")
    }
    old_candidate = GATE._STAGED_OWNER_CANDIDATE_AUTHORITY
    old_configured = GATE._RUNTIME_EXE_CONFIGURED
    old_error = GATE._RUNTIME_EXE_ERROR
    try:
        manifest_path = Path(str(manifest["directory"])) / RELEASE_GATE.RELEASE_MANIFEST_FILE
        with mock.patch.object(
            GATE,
            "_canonical_live_data_roots",
            return_value=(appdata.resolve(), root.parent.parent.resolve()),
        ), mock.patch.dict(
            os.environ,
            {
                "APPDATA": str(appdata),
                "LOCALAPPDATA": str(root.parent.parent),
                "CORTEX_APP_EXE": str(Path(temporary.name) / "malicious.exe"),
            },
            clear=False,
        ):
            authority = GATE._prepare_run_authority(
                False,
                expected_sha=GIT_SHA,
                staged_candidate_manifest=manifest_path,
                release_phase=GATE.RELEASE_PHASE_PREDEPLOYMENT,
            )
            mode, _digest = GATE._validate_run_authority(authority)
            assert mode == GATE.AUTHORITY_MODE_STAGED_CANDIDATE
            assert authority["certificationEligible"] is False
            assert authority["releasePhase"] == GATE.RELEASE_PHASE_PREDEPLOYMENT
            assert authority["stagedCandidate"]["releaseId"] == manifest["releaseId"]
            assert authority["stagedCandidate"]["manifestSha256"] == GATE.sha256_file(
                manifest_path
            )
            assert authority["stagedCandidate"]["artifacts"]["operationsBundle"][
                "sha256"
            ] == manifest["operationsSha256"]
            assert authority["stagedCandidate"]["artifacts"]["dedupManifest"][
                "declaredSha256"
            ] == manifest["dedupManifestSha256"]
            boolean_schema = json.loads(json.dumps(authority["stagedCandidate"]))
            boolean_schema["schema"] = True
            try:
                GATE._validate_staged_candidate_authority(boolean_schema)
            except GATE.EvidenceError as error:
                assert "invalid release identity" in str(error)
            else:
                raise AssertionError("boolean schema value impersonated staged schema 1")
            assert os.environ["CORTEX_APP_EXE"] == str(exe.resolve())
            assert "CORTEX_APP_EXE" in authority["callerOverrides"]["names"]

            recorded = GATE._release_artifact_bindings(GIT_SHA)
            assert recorded[0]["authority"] == "staged-owner-candidate"
            assert recorded[0]["stagedReleaseId"] == manifest["releaseId"]
            assert recorded[0]["stagedReleaseManifestSha256"] == GATE.sha256_file(
                manifest_path
            )
            GATE._validate_release_artifacts(
                GATE.PROFILE_OWNER,
                recorded,
                GIT_SHA,
                eligible=False,
                run_authority=authority,
            )
            code, verdict = GATE._profile_verdict(
                GATE.PROFILE_OWNER,
                False,
                [],
                [],
                staged_candidate=True,
            )
            assert code == 2 and "pre-deployment staged-candidate" in verdict
            try:
                GATE._require_certifying_manifest(
                    {
                        "profile": GATE.PROFILE_OWNER,
                        "quick": False,
                        "certificationEligible": False,
                        "exitCode": 2,
                        "requiredEvidencePending": [],
                    },
                    GATE.PROFILE_OWNER,
                )
            except GATE.EvidenceError as error:
                assert "not certification-eligible" in str(error)
            else:
                raise AssertionError("staged candidate was accepted as final certification")
    finally:
        GATE._STAGED_OWNER_CANDIDATE_AUTHORITY = old_candidate
        GATE._RUNTIME_EXE_CONFIGURED = old_configured
        GATE._RUNTIME_EXE_ERROR = old_error
        for key, value in old_environment.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        temporary.cleanup()


def test_staged_candidate_manifest_is_an_explicit_owner_predeploy_cli_authority() -> None:
    manifest_path = Path(r"C:\sealed\release\release-manifest.json")
    with mock.patch.object(
        sys,
        "argv",
        [
            "verify_10.py",
            "--profile",
            GATE.PROFILE_OWNER,
            "--staged-owner-candidate-manifest",
            str(manifest_path),
        ],
    ), mock.patch.object(GATE, "aggregate_main", return_value=2) as aggregate:
        assert GATE.main() == 2
    aggregate.assert_called_once_with(
        quick=False,
        status_md=None,
        profile=GATE.PROFILE_OWNER,
        diagnostic_live_authority_overrides=False,
        staged_owner_candidate_manifest=manifest_path,
        owner_release_phase=GATE.RELEASE_PHASE_PREDEPLOYMENT,
    )


def test_staged_candidate_wrong_root_symlink_schema_sha_and_hash_fail_closed() -> None:
    temporary, _appdata, root, manifest, exe = fixture()
    try:
        manifest_path = Path(str(manifest["directory"])) / RELEASE_GATE.RELEASE_MANIFEST_FILE
        try:
            GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha=GIT_SHA, release_root=root.parent
            )
        except GATE.EvidenceError as error:
            assert "canonical release root" in str(error)
        else:
            raise AssertionError("candidate outside the configured release root was accepted")

        with mock.patch.object(Path, "is_symlink", autospec=True, return_value=True):
            try:
                GATE.validate_staged_owner_candidate_manifest(
                    manifest_path, expected_sha=GIT_SHA, release_root=root
                )
            except GATE.EvidenceError as error:
                assert "non-symlink" in str(error)
            else:
                raise AssertionError("symlink candidate manifest was accepted")

        try:
            GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha="b" * 40, release_root=root
            )
        except GATE.EvidenceError as error:
            assert "another Git commit" in str(error)
        else:
            raise AssertionError("candidate from another source SHA was accepted")

        original_manifest = manifest_path.read_bytes()
        changed = dict(manifest, expectedDatabaseSchema=68)
        manifest_path.write_text(json.dumps(changed), encoding="utf-8")
        try:
            GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha=GIT_SHA, release_root=root
            )
        except GATE.EvidenceError as error:
            assert "schema 69" in str(error) or "database schema 69" in str(error)
        else:
            raise AssertionError("candidate with the wrong database schema was accepted")
        manifest_path.write_bytes(original_manifest)

        exe.write_bytes(exe.read_bytes() + b"tampered")
        try:
            GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha=GIT_SHA, release_root=root
            )
        except GATE.EvidenceError as error:
            assert "hash" in str(error).lower() or "sha-256" in str(error).lower()
        else:
            raise AssertionError("candidate with changed executable bytes was accepted")
    finally:
        temporary.cleanup()


def test_staged_candidate_mutation_invalidates_revalidation_and_latest_binding() -> None:
    temporary, appdata, root, manifest, _exe = fixture()
    old_candidate = GATE._STAGED_OWNER_CANDIDATE_AUTHORITY
    try:
        manifest_path = Path(str(manifest["directory"])) / RELEASE_GATE.RELEASE_MANIFEST_FILE
        authority = GATE.validate_staged_owner_candidate_manifest(
            manifest_path, expected_sha=GIT_SHA, release_root=root
        )
        with mock.patch.object(
            GATE,
            "_canonical_live_data_roots",
            return_value=(appdata.resolve(), root.parent.parent.resolve()),
        ):
            run_authority = GATE._run_authority_document(
                diagnostic_overrides=False,
                staged_candidate=authority,
                release_phase=GATE.RELEASE_PHASE_PREDEPLOYMENT,
            )
            GATE._STAGED_OWNER_CANDIDATE_AUTHORITY = authority
            recorded = GATE._release_artifact_bindings(GIT_SHA)
            GATE._revalidate_latest_release_executable(
                GATE.PROFILE_OWNER,
                recorded,
                GIT_SHA,
                run_authority,
            )
            manifest_path.write_bytes(manifest_path.read_bytes() + b"\n")
            try:
                GATE._revalidate_latest_release_executable(
                    GATE.PROFILE_OWNER,
                    recorded,
                    GIT_SHA,
                    run_authority,
                )
            except GATE.EvidenceError as error:
                assert "changed" in str(error)
            else:
                raise AssertionError("mutated candidate manifest retained latest-proof authority")
    finally:
        GATE._STAGED_OWNER_CANDIDATE_AUTHORITY = old_candidate
        temporary.cleanup()


def test_every_staged_candidate_artifact_is_reobserved_after_authority_capture() -> None:
    relative_paths = (
        "cortex-speech-app.exe",
        "pool_admin.exe",
        "scripts/ops/cortex-watchdog.ps1",
        "scripts/release_private_production.py",
        RELEASE_GATE.DEDUP_MANIFEST_FILE,
        str(RELEASE_GATE.SCHEMA_CONTRACT_RELATIVE_PATH),
    )
    for relative in relative_paths:
        temporary, _appdata, root, manifest, _exe = fixture()
        try:
            manifest_path = (
                Path(str(manifest["directory"])) / RELEASE_GATE.RELEASE_MANIFEST_FILE
            )
            authority = GATE.validate_staged_owner_candidate_manifest(
                manifest_path, expected_sha=GIT_SHA, release_root=root
            )
            artifact = Path(str(manifest["directory"])) / relative
            suffix = b"\n  " if artifact.suffix == ".json" else b"\nmutated-after-capture\n"
            artifact.write_bytes(artifact.read_bytes() + suffix)
            try:
                GATE._revalidate_staged_candidate_authority(
                    authority, release_root=root
                )
            except GATE.EvidenceError:
                pass
            else:
                raise AssertionError(
                    f"mutated staged authority artifact retained proof authority: {relative}"
                )
        finally:
            temporary.cleanup()


def test_staged_runtime_gates_use_bound_executable_without_caller_or_active_pointer() -> None:
    playback = GATE._gate_by_id("playback-enforcement-readiness")
    freshness = GATE._gate_by_id("exe-freshness")
    # _gate_environment inventories caller overrides against the canonical live data roots,
    # which resolve only through the Windows Known Folder API (no env override, by design).
    # The environment-composition assertions below are platform-independent, so on POSIX stub
    # that single seam with synthetic roots; Windows runs the real resolution unpatched.
    with tempfile.TemporaryDirectory() as raw:
        live_roots = (
            contextlib.nullcontext()
            if os.name == "nt"
            else mock.patch.object(
                GATE,
                "_canonical_live_data_roots",
                return_value=(Path(raw) / "live-roaming", Path(raw) / "live-local"),
            )
        )
        with live_roots, mock.patch.dict(
            os.environ, {"CORTEX_APP_EXE": r"C:\sealed\cortex-speech-app.exe"}
        ):
            steps = GATE._effective_gate_steps(
                playback.id, playback.steps, GATE.AUTHORITY_MODE_STAGED_CANDIDATE
            )
            staged_freshness = GATE._gate_environment(
                freshness, GATE.AUTHORITY_MODE_STAGED_CANDIDATE
            )
            live_freshness = GATE._gate_environment(freshness, GATE.AUTHORITY_MODE_LIVE)
    argv = list(steps[0].argv)
    assert "--active-release" not in argv
    assert argv[-2:] == ["--exe", r"C:\sealed\cortex-speech-app.exe"]
    assert staged_freshness["CORTEX_APP_EXE"] == r"C:\sealed\cortex-speech-app.exe"
    assert "CORTEX_APP_EXE" not in live_freshness


def test_latest_proof_reobserves_active_executable_after_measurement() -> None:
    temporary, appdata, root, manifest, exe = fixture()
    old_environment = {
        key: os.environ.get(key) for key in ("APPDATA", "LOCALAPPDATA", "CORTEX_APP_EXE")
    }
    old_configured = GATE._RUNTIME_EXE_CONFIGURED
    old_error = GATE._RUNTIME_EXE_ERROR
    old_candidate = GATE._STAGED_OWNER_CANDIDATE_AUTHORITY
    try:
        pointer = appdata / "cortex-speech" / GATE.ACTIVE_RELEASE_POINTER
        pointer.parent.mkdir(parents=True)
        pointer.write_text(json.dumps(manifest), encoding="utf-8")
        os.environ["APPDATA"] = str(appdata)
        os.environ["LOCALAPPDATA"] = str(root.parent.parent)
        os.environ.pop("CORTEX_APP_EXE", None)
        GATE._RUNTIME_EXE_CONFIGURED = False
        GATE._RUNTIME_EXE_ERROR = None
        GATE._STAGED_OWNER_CANDIDATE_AUTHORITY = None

        recorded = GATE._release_artifact_bindings(GIT_SHA)
        assert recorded[0]["authority"] == "active-immutable-release"
        GATE._revalidate_latest_release_executable(
            GATE.PROFILE_OWNER,
            recorded,
            GIT_SHA,
        )

        exe.write_bytes(exe.read_bytes() + b"tampered-after-proof")
        try:
            GATE._revalidate_latest_release_executable(
                GATE.PROFILE_OWNER,
                recorded,
                GIT_SHA,
            )
        except GATE.EvidenceError as error:
            assert "changed after measurement" in str(error)
        else:
            raise AssertionError("latest-proof accepted an executable changed after measurement")
    finally:
        GATE._RUNTIME_EXE_CONFIGURED = old_configured
        GATE._RUNTIME_EXE_ERROR = old_error
        GATE._STAGED_OWNER_CANDIDATE_AUTHORITY = old_candidate
        for key, value in old_environment.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        temporary.cleanup()


if __name__ == "__main__":
    tests = [
        value
        for name, value in sorted(globals().items())
        if name.startswith("test_") and callable(value)
    ]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"verify-10 release-authority regressions passed ({len(tests)} assertions)")
