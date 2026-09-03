"""Fault and trust-boundary regressions for the verify-10 supervisor."""

from __future__ import annotations

import importlib.util
import io
import errno
import ctypes
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from dataclasses import replace
from ctypes import wintypes
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
VERIFY = REPO_ROOT / "scripts" / "verify_10.py"
SUPERVISOR = REPO_ROOT / "scripts" / "verify10_supervisor.py"
ASSERT_RAN = REPO_ROOT / "cortex-speech-app" / "scripts" / "assert_ran.py"
VITEST_CONFIG = REPO_ROOT / "cortex-speech-app" / "vitest.config.ts"
FRONTEND_COVERAGE_CONTRACT = REPO_ROOT / "cortex-speech-app" / "scripts" / "frontend_coverage_contract.v1.json"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


# These tests drive full verifier runs (aggregate_main / _run_authority_document) whose run
# authority binds the live product through SHGetKnownFolderPath — deliberately without any
# environment override (that refusal is itself a pinned property). No portable authority exists
# by design, so they execute only where the Windows Known Folder API does. The Windows Release
# Gate runs them unskipped.
_requires_windows_live_authority = unittest.skipUnless(
    sys.platform == "win32",
    "full verifier runs bind live product authority via Windows Known Folder resolution (SHGetKnownFolderPath)",
)


class Verify10SupervisorTests(unittest.TestCase):
    def _environment_document_without_toolchain_probe(self) -> dict[str, object]:
        """The supervisor's environment document, minus the part that needs a real toolchain.

        `_rust_coverage_environment_document()` probes rustc, cargo, the date-pinned coverage nightly
        via rustup, and cargo-llvm-cov. The Linux and macOS smoke runners install none of those, so
        the real call raises EvidenceError BEFORE the spawn/wait mocks are ever reached and the
        supervisor correctly records VERIFIER_FAILURE -- which is not what the terminal-pointer tests
        are about. Measured 2026-09-02: both green on the workstation (nightly installed), red on both
        smoke runners, reproduced under WSL. Host, python and the contract-derived toolchain identity
        stay real; only the four executable probes become labelled placeholders.
        """
        python_path = Path(sys.executable).resolve(strict=True)

        def placeholder(name: str) -> dict[str, object]:
            return {"name": name, "resolved": None, "sha256": None, "bytes": None, "version": "not probed in unit test"}

        return {
            "schema": 1,
            "host": self.verify._environment_document(),
            "python": {
                "name": python_path.name,
                "sha256": self.verify.sha256_file(python_path),
                "bytes": python_path.stat().st_size,
                "version": sys.version,
            },
            "productionRustc": placeholder("rustc"),
            "productionCargo": placeholder("cargo"),
            "coverageToolchain": self.verify._expected_rust_coverage_toolchain_identity(),
            "coverageRustc": placeholder("rustc"),
            "coverageCargo": placeholder("cargo"),
            "cargoLlvmCov": placeholder("cargo-llvm-cov"),
            "networkPolicy": "unit test: no toolchain probe",
        }

    @classmethod
    def setUpClass(cls) -> None:
        cls.supervisor = load_module("verify10_supervisor_fault_test", SUPERVISOR)
        cls.verify = load_module("verify10_fault_test", VERIFY)

    def test_frontend_snapshot_matches_node_case_sensitive_lexical_order(self) -> None:
        app_svelte = self.verify.APP / "src" / "App.svelte"
        app_css = self.verify.APP / "src" / "app.css"
        rows, digest = self.verify._frontend_snapshot([app_css, app_svelte])
        expected = [
            {
                "path": "src/App.svelte",
                "sha256": self.verify.sha256_file(app_svelte),
            },
            {
                "path": "src/app.css",
                "sha256": self.verify.sha256_file(app_css),
            },
        ]
        expected_digest = hashlib.sha256(
            "\n".join(
                f"{item['path']}\0{item['sha256']}" for item in expected
            ).encode("utf-8")
        ).hexdigest()
        self.assertEqual(rows, expected)
        self.assertEqual(digest, expected_digest)

    def test_owner_evidence_paths_reject_symlink_and_windows_junction_aliases(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            alias = root / "alias"
            alias.mkdir()
            relative = self.verify.PurePosixPath("alias/evidence.json")

            with mock.patch.object(
                Path,
                "is_symlink",
                lambda path: path == alias,
            ):
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "link or junction"
                ):
                    self.verify._owner_evidence_path(root, relative)

            with mock.patch.object(
                Path,
                "is_junction",
                lambda path: path == alias,
                create=True,
            ):
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "link or junction"
                ):
                    self.verify._owner_evidence_path(root, relative)
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "contains a link"
                ):
                    self.verify._owner_campaign_file_inventory(root)

            outside = root.parent / f"{root.name}-hardlink-source.bin"
            outside.write_bytes(b"shared evidence bytes")
            hardlink = root / "hardlink.bin"
            os.link(outside, hardlink)
            try:
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "hard-link alias"
                ):
                    self.verify._owner_campaign_file_inventory(root)
            finally:
                hardlink.unlink()
                outside.unlink()

    def test_owner_evidence_relative_paths_reject_windows_alias_spellings(self) -> None:
        for value in (
            "folder//artifact.json",
            "folder/./artifact.json",
            "artifact.json.",
            "artifact.json ",
            "stream:payload",
            "CON.txt",
            "LPT9",
            "control\x1fbyte.json",
        ):
            with self.subTest(value=value), self.assertRaises(
                self.verify.EvidenceError
            ):
                self.verify._safe_owner_evidence_relative(value, label="test path")

    def _llvm_coverage_payload(
        self,
        *,
        covered: int = 95,
        count: int = 100,
        branch_count: int | None = None,
    ) -> bytes:
        branch_count = count if branch_count is None else branch_count
        totals = {}
        for name, metric_count in (
            ("lines", count),
            ("regions", count),
            ("functions", count),
            ("branches", branch_count),
        ):
            metric_covered = min(covered, metric_count)
            totals[name] = {
                "count": metric_count,
                "covered": metric_covered,
                "percent": (metric_covered * 100.0 / metric_count) if metric_count else 0.0,
            }
        quality = self.verify._rust_quality_module()
        critical_files = []
        representative_paths = {
            pattern.replace("*.rs", "fixture.rs")
            for patterns in quality.CRITICAL_COVERAGE_DOMAINS.values()
            for pattern in patterns
        }
        for path in sorted(representative_paths):
            summary = {}
            for name, metric_count in (
                ("lines", count),
                ("regions", count),
                ("functions", count),
                ("branches", branch_count),
            ):
                metric_covered = min(95 if name != "branches" else 90, metric_count)
                summary[name] = {
                    "count": metric_count,
                    "covered": metric_covered,
                    "percent": (
                        metric_covered * 100.0 / metric_count if metric_count else 0.0
                    ),
                }
            critical_files.append({"filename": path, "summary": summary})
        return json.dumps(
            {
                "type": "llvm.coverage.json.export",
                "version": "2.0.1",
                "data": [{"totals": totals, "files": critical_files}],
            },
            sort_keys=True,
        ).encode("utf-8")

    def _write_coverage_phase(
        self,
        root: Path,
        *,
        sha: str,
        checkout_digest: str,
        ended: datetime | None = None,
    ) -> tuple[Path, dict[str, object]]:
        token = hashlib.sha256(str(root).encode("utf-8")).hexdigest()[:32]
        phase_dir = root / token
        phase_dir.mkdir(parents=True, exist_ok=False)
        artifact_path = phase_dir / self.verify.RUST_COVERAGE_ARTIFACT_NAME
        artifact_path.write_bytes(self._llvm_coverage_payload())
        (phase_dir / "worker.log").write_text("one exact successful attempt\n", encoding="utf-8")
        toolchain = self.verify._expected_rust_coverage_toolchain_identity()
        report = self.verify._rust_coverage_report(
            artifact_path,
            coverage_toolchain=toolchain,
        )
        ended = ended or datetime.now(timezone.utc).replace(microsecond=0)
        started = ended - timedelta(minutes=5)
        registry = self.verify._rust_coverage_command_registry()
        events = [
            {
                "schema": 1,
                "sequence": 1,
                "runToken": token,
                "event": "phase_start",
                "at": self.verify._format_utc(started),
                "fullGitSha": sha,
                "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                "checkoutStateDigest": checkout_digest,
                "commandRegistryHash": registry["registrySha256"],
            },
            {
                "schema": 1,
                "sequence": 2,
                "runToken": token,
                "event": "phase_end",
                "at": self.verify._format_utc(ended),
                "exitCode": 0,
                "verdict": "PASS",
                "artifactSha256": report["artifactSha256"],
            },
        ]
        (phase_dir / "events.jsonl").write_text(
            "".join(json.dumps(event, sort_keys=True) + "\n" for event in events),
            encoding="utf-8",
            newline="\n",
        )
        manifest = {
            "schema": 1,
            "type": "RustCoveragePrerequisiteV1",
            "complete": True,
            "runToken": token,
            "fullGitSha": sha,
            "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
            "checkoutStateDigest": checkout_digest,
            "startedAt": self.verify._format_utc(started),
            "endedAt": self.verify._format_utc(ended),
            "expiresAt": self.verify._format_utc(
                ended + timedelta(seconds=self.verify.RUST_COVERAGE_FRESH_SECONDS)
            ),
            "exitCode": 0,
            "attemptCount": 1,
            "commandRegistry": registry,
            "environment": {
                "schema": 1,
                "synthetic": "unit-test",
                "coverageToolchain": toolchain,
            },
            "coverage": report,
            "artifacts": self.verify._rust_coverage_phase_artifacts(phase_dir),
        }
        manifest_path = phase_dir / self.verify.RUST_COVERAGE_MANIFEST_NAME
        self.supervisor.atomic_write_json(manifest_path, manifest)
        return manifest_path, manifest

    def _synthetic_coverage_binding(
        self,
        run_dir: Path,
        *,
        expected_sha: str,
        expected_checkout_digest: str,
    ) -> dict[str, object]:
        manifest_path, phase = self._write_coverage_phase(
            run_dir / "prerequisites" / self.verify.RUST_COVERAGE_PHASE_DIRNAME,
            sha=expected_sha,
            checkout_digest=expected_checkout_digest,
            ended=datetime.now(timezone.utc).replace(microsecond=0) - timedelta(minutes=1),
        )
        return {
            "path": str(manifest_path.relative_to(run_dir)),
            "sha256": self.supervisor.sha256_file(manifest_path),
            "bytes": manifest_path.stat().st_size,
            "runToken": phase["runToken"],
            "fullGitSha": expected_sha,
            "artifactSha256": phase["coverage"]["artifactSha256"],
            "completedAt": phase["endedAt"],
            "expiresAt": phase["expiresAt"],
            "commandRegistryHash": phase["commandRegistry"]["registrySha256"],
        }

    @staticmethod
    def _stable_active_release_binding(full_sha: str) -> list[dict[str, object]]:
        """Keep aggregate unit tests independent from a concurrently rebuilt workspace binary."""

        return [
            {
                "role": "application-executable",
                "name": "cortex-speech-app.exe",
                "sha256": "a" * 64,
                "bytes": 42,
                "buildGitSha": full_sha,
                "matchesFullGitSha": True,
                "authority": "active-immutable-release",
                "activeReleasePointerSha256": "b" * 64,
                "activeReleaseGitSha": full_sha,
            }
        ]

    def _write_owner_campaign_shell(
        self,
        root: Path,
        class_id: str,
        *,
        sha: str,
        registry_hash: str,
        checkout_digest: str,
        environment: dict[str, object],
        token_suffix: int,
    ) -> Path:
        token = f"{token_suffix:032x}"
        campaign = root / class_id / token
        campaign.mkdir(parents=True)
        started = datetime.now(timezone.utc).replace(microsecond=0) - timedelta(minutes=2)
        ended = started + timedelta(minutes=1)
        expires = ended + timedelta(
            seconds=self.verify._owner_campaign_fresh_seconds(class_id)
        )
        for name in self.verify.OWNER_EVIDENCE_RAW_ARTIFACTS[class_id]:
            path = campaign.joinpath(*Path(name).parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            if name == self.verify.OWNER_EVIDENCE_SOURCE_EVENTS:
                continue
            path.write_text("{}\n", encoding="utf-8")
        events = [
            {
                "schema": 1,
                "sequence": 1,
                "runToken": token,
                "event": "campaign_start",
                "at": self.verify._format_utc(started),
                "classId": class_id,
                "profile": self.verify.PROFILE_OWNER,
                "fullGitSha": sha,
                "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                "checkoutStateDigest": checkout_digest,
                "gateRegistryHash": registry_hash,
                "environmentDigest": self.verify._document_digest(environment),
                "attemptCount": 1,
                "retryPolicy": "none",
            },
            {
                "schema": 1,
                "sequence": 2,
                "runToken": token,
                "event": "campaign_end",
                "at": self.verify._format_utc(ended),
                "classId": class_id,
                "passed": True,
                "failures": [],
                "retryCount": 0,
                "skipCount": 0,
            },
        ]
        events_path = campaign / self.verify.OWNER_EVIDENCE_SOURCE_EVENTS
        events_path.write_text(
            "".join(json.dumps(item, sort_keys=True) + "\n" for item in events),
            encoding="utf-8",
            newline="\n",
        )
        artifacts = []
        for name in self.verify.OWNER_EVIDENCE_RAW_ARTIFACTS[class_id]:
            path = campaign.joinpath(*Path(name).parts)
            artifacts.append(
                {
                    "path": name,
                    "sha256": self.supervisor.sha256_file(path),
                    "bytes": path.stat().st_size,
                }
            )
        manifest = {
            "schema": 1,
            "type": self.verify.OWNER_EVIDENCE_SOURCE_TYPES[class_id],
            "classId": class_id,
            "runToken": token,
            "profile": self.verify.PROFILE_OWNER,
            "fullGitSha": sha,
            "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
            "gateRegistryHash": registry_hash,
            "checkoutStateDigest": checkout_digest,
            "environmentDigest": self.verify._document_digest(environment),
            "startedAt": self.verify._format_utc(started),
            "endedAt": self.verify._format_utc(ended),
            "expiresAt": self.verify._format_utc(expires),
            "attemptCount": 1,
            "retryCount": 0,
            "skipCount": 0,
            "artifacts": artifacts,
            "passed": True,
            "failures": [],
        }
        manifest_path = campaign / self.verify.OWNER_EVIDENCE_SOURCE_MANIFEST
        self.supervisor.atomic_write_json(manifest_path, manifest)
        return manifest_path

    def _write_owner_proof_fixture(self, root: Path, sha: str) -> dict[str, Path]:
        root.mkdir(parents=True, exist_ok=True)
        contract_source = REPO_ROOT / "cortex-speech-app" / "scripts" / "owner_proof_input_contract.v1.json"
        contract = root / "owner_proof_input_contract.v1.json"
        shutil.copyfile(contract_source, contract)
        required_roles = [
            "proof-input-contract",
            "real-media-mp4",
            "real-media-mov",
            "real-media-flac",
            "long-audiobook-mp3",
            "scale-database-authority",
            "campaign-database-authority",
            "scale-database-derived-current",
            "database-migration-helper",
            "database-migration-helper-source",
        ]
        files = []
        for index, role in enumerate(required_roles, start=1):
            files.append(
                {
                    "role": role,
                    "relativePath": f"fixtures/{index}-{role}",
                    "sha256": (
                        self.supervisor.sha256_file(contract)
                        if role == "proof-input-contract"
                        else hashlib.sha256(role.encode()).hexdigest()
                    ),
                    "sizeBytes": contract.stat().st_size if role == "proof-input-contract" else index,
                    "readOnlyHashBound": True,
                }
            )
        manifest = {
            "schema": 1,
            "bundleId": "cortex-owner-product-proof-inputs-v1",
            "releaseGitSha": sha,
            "contractSha256": self.supervisor.sha256_file(contract),
            "helperSha256": "a" * 64,
            "helperSourceSha256": "b" * 64,
            "helperBuild": {},
            "files": files,
            "sourcePreservation": {},
            "databases": {},
            "safety": {},
        }
        manifest_path = root / "manifest.v1.json"
        self.supervisor.atomic_write_json(manifest_path, manifest)
        return {
            "owner-proof/manifest.v1.json": manifest_path,
            "owner-proof/owner_proof_input_contract.v1.json": contract,
        }

    def _write_fault_campaign(
        self,
        root: Path,
        *,
        token: str,
        started: datetime,
        sha: str,
        registry_hash: str,
        checkout_digest: str,
        environment: dict[str, object],
    ) -> Path:
        run_dir = root / token
        run_dir.mkdir(parents=True, exist_ok=False)
        ended = started + timedelta(minutes=1)
        started_at = self.verify._format_utc(started)
        ended_at = self.verify._format_utc(ended)
        environment_digest = self.verify._document_digest(environment)
        start = {
            "schema": 1,
            "type": "VerifierFaultCampaignStartV1",
            "runToken": token,
            "fullGitSha": sha,
            "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
            "checkoutStateDigest": checkout_digest,
            "gateRegistryHash": registry_hash,
            "environmentDigest": environment_digest,
            "startedAt": started_at,
            "attemptCount": 1,
            "retryPolicy": "none",
        }
        self.supervisor.atomic_write_json(
            run_dir / self.verify.VERIFIER_FAULT_CAMPAIGN_START, start
        )
        events = [
            {
                "schema": 1,
                "sequence": 1,
                "runToken": token,
                "event": "campaign_start",
                "at": started_at,
                "fullGitSha": sha,
                "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                "checkoutStateDigest": checkout_digest,
                "gateRegistryHash": registry_hash,
                "environmentDigest": environment_digest,
                "attemptCount": 1,
                "retryPolicy": "none",
            },
            {
                "schema": 1,
                "sequence": 2,
                "runToken": token,
                "event": "campaign_end",
                "at": ended_at,
                "exitCode": 0,
                "passed": True,
                "retryCount": 0,
                "failureCount": 0,
            },
        ]
        (run_dir / "events.jsonl").write_text(
            "".join(json.dumps(event, sort_keys=True) + "\n" for event in events),
            encoding="utf-8",
            newline="\n",
        )
        test_results = [
            {"name": name, "outcome": "ok"}
            for name in self.verify.VERIFIER_FAULT_TEST_METHODS
        ]
        log_lines = [
            f"{name} (test_verify10_supervisor.Verify10SupervisorTests.{name}) ... ok"
            for name in self.verify.VERIFIER_FAULT_TEST_METHODS
        ]
        log_lines.extend(
            [
                "----------------------------------------------------------------------",
                f"Ran {len(test_results)} tests in 1.000s",
                "",
                "OK",
            ]
        )
        (run_dir / self.verify.VERIFIER_FAULT_CAMPAIGN_LOG).write_text(
            "\n".join(log_lines) + "\n", encoding="utf-8", newline="\n"
        )
        manifest = {
            "schema": 1,
            "type": "VerifierFaultCampaignV1",
            "complete": True,
            "runToken": token,
            "fullGitSha": sha,
            "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
            "checkoutStateDigest": checkout_digest,
            "gateRegistryHash": registry_hash,
            "environment": environment,
            "environmentDigest": environment_digest,
            "startedAt": started_at,
            "endedAt": ended_at,
            "expiresAt": self.verify._format_utc(
                ended
                + timedelta(seconds=self.verify.VERIFIER_FAULT_CAMPAIGN_FRESH_SECONDS)
            ),
            "attemptCount": 1,
            "retryCount": 0,
            "command": {
                "argv": self.verify._verifier_fault_campaign_command(),
                "cwd": str(self.verify._fault_campaign_test_source().parent.resolve()),
                "forcedEnvironment": {
                    "PYTHONIOENCODING": "utf-8",
                    "PYTHONUTF8": "1",
                },
            },
            "testSource": self.verify._tracked_authority_binding(
                self.verify._fault_campaign_test_source(), sha
            ),
            "testResults": test_results,
            "scenarioResults": self.verify._fault_scenario_results(test_results),
            "residuals": {
                "processIdentities": [],
                "occupiedDevelopmentPorts": [],
                "leasePaths": [],
                "partialStatusPointers": [],
            },
            "exitCode": 0,
            "passed": True,
            "failures": [],
            "artifacts": self.verify._fault_campaign_artifacts(run_dir),
        }
        manifest_path = run_dir / self.verify.VERIFIER_FAULT_CAMPAIGN_MANIFEST
        self.supervisor.atomic_write_json(manifest_path, manifest)
        return manifest_path

    def test_registry_is_typed_profiled_explicit_and_below_six_hours(self) -> None:
        gates = self.verify.GATES
        self.assertTrue(gates)
        self.assertEqual(len({gate.id for gate in gates}), len(gates))
        self.assertTrue(all(isinstance(gate, self.verify.GateSpec) for gate in gates))
        self.assertTrue(all(gate.timeout_seconds > 0 for gate in gates))
        self.assertTrue(all(gate.profiles and gate.profiles <= self.verify.PROFILES for gate in gates))
        self.assertTrue(
            all(step.argv and all(isinstance(arg, str) and arg for arg in step.argv) for gate in gates for step in gate.steps)
        )
        full_budget = sum(gate.timeout_seconds for gate in gates if self.verify.PROFILE_FULL in gate.profiles)
        self.assertLessEqual(full_budget, 6 * 60 * 60)
        clippy_argv = [argument for step in self.verify._gate_by_id("clippy").steps for argument in step.argv]
        rust_test_argv = [
            argument for step in self.verify._gate_by_id("test-rust").steps for argument in step.argv
        ]
        for gate_id, argv in (("clippy", clippy_argv), ("test-rust", rust_test_argv)):
            self.assertIn("--all-targets", argv, gate_id)
            self.assertIn("--all-features", argv, gate_id)
        for gate_id, minimum in (("test-frontend", 400), ("test-rust", 1700)):
            argv = [
                argument
                for step in self.verify._gate_by_id(gate_id).steps
                for argument in step.argv
            ]
            min_index = argv.index("--min")
            self.assertGreaterEqual(int(argv[min_index + 1]), minimum, gate_id)
        frontend_coverage = self.verify._gate_by_id("frontend-coverage")
        self.assertTrue(
            {self.verify.PROFILE_OWNER, self.verify.PROFILE_WINDOWS}
            <= frontend_coverage.profiles
        )
        frontend_coverage_argv = [
            argument for step in frontend_coverage.steps for argument in step.argv
        ]
        self.assertTrue(
            any("test:coverage" in argument for argument in frontend_coverage_argv),
            frontend_coverage_argv,
        )
        coverage_config = VITEST_CONFIG.read_text(encoding="utf-8")
        coverage_contract = json.loads(FRONTEND_COVERAGE_CONTRACT.read_text(encoding="utf-8"))
        self.assertEqual(
            coverage_contract["thresholds"],
            {"statements": 85, "branches": 80, "functions": 80, "lines": 85},
        )
        self.assertIn("frontend_coverage_contract.v1.json", coverage_config)
        self.assertIn("...coverageContract.thresholds", coverage_config)
        self.assertIn("json-summary", coverage_config)
        source = VERIFY.read_text(encoding="utf-8")
        self.assertNotIn("shell=True", source)
        assert_ran_source = ASSERT_RAN.read_text(encoding="utf-8")
        self.assertNotIn("shell=True", assert_ran_source)
        self.assertIn("subprocess.run(argv, shell=False", assert_ran_source)
        self.assertIn('[command_processor, "/d", "/s", "/c"', assert_ran_source)

        architecture = self.verify._gate_by_id("rust-architecture-truth")
        self.assertTrue({self.verify.PROFILE_OWNER, self.verify.PROFILE_WINDOWS} <= architecture.profiles)
        self.assertFalse(any(gate.id == "rust-coverage-truth" for gate in gates))
        coverage_registry = self.verify._rust_coverage_command_registry()
        self.assertEqual(coverage_registry["retryPolicy"], "none")
        self.assertIn("--branch", coverage_registry["measurementArgv"])
        self.assertIn("--all-targets", coverage_registry["measurementArgv"])
        self.assertIn("--all-features", coverage_registry["measurementArgv"])
        self.assertIn("+nightly-2026-07-11", coverage_registry["measurementArgv"])
        self.assertNotIn("+nightly", coverage_registry["measurementArgv"])
        self.assertEqual(
            coverage_registry["criticalDomainThresholds"],
            {
                "lines": 95.0,
                "regions": 95.0,
                "functions": 90.0,
                "branches": 90.0,
            },
        )
        contract = coverage_registry["coverageToolchainContract"]
        self.assertEqual(contract["toolchain"], "nightly-2026-07-11")
        self.assertRegex(str(contract["sha256"]), r"^[0-9a-f]{64}$")
        gate_registry = {
            item["id"]: item for item in self.verify.gate_registry_document()["gates"]
        }
        self.assertEqual(
            gate_registry["champion-7b-preflight"]["forcedEnvironment"],
            {"CORTEX_REQUIRE_7B": "1"},
        )

    def test_failed_rust_coverage_prerequisite_replaces_running_pointer_with_terminal_evidence(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = (
                self.verify.RUST_COVERAGE_PHASE_ROOT,
                self.verify.RUST_COVERAGE_LATEST,
                self.verify.RUST_COVERAGE_LOCK,
            )
            try:
                self.verify.RUST_COVERAGE_PHASE_ROOT = root / "phases"
                self.verify.RUST_COVERAGE_LATEST = (
                    root / "latest-rust-coverage-prerequisite.json"
                )
                self.verify.RUST_COVERAGE_LOCK = (
                    root / "rust-coverage-prerequisite.lease.json"
                )
                process = mock.Mock(pid=os.getpid())
                with mock.patch.object(
                    self.verify,
                    "_rust_coverage_environment_document",
                    side_effect=self._environment_document_without_toolchain_probe,
                ), mock.patch.object(
                    self.verify,
                    "spawn_isolated",
                    return_value=(process, mock.Mock()),
                ), mock.patch.object(
                    self.verify,
                    "wait_isolated",
                    return_value=(7, False),
                ):
                    code = self.verify.rust_coverage_prerequisite_main()

                self.assertEqual(code, 1)
                pointer = json.loads(
                    self.verify.RUST_COVERAGE_LATEST.read_text(encoding="utf-8")
                )
                self.assertEqual(pointer["state"], "FAILED")
                self.assertEqual(pointer["verdict"], "FAIL")
                self.assertEqual(pointer["terminalEvent"], "phase_end")
                self.assertEqual(pointer["exitCode"], 1)
                self.assertEqual(pointer["childExitCode"], 7)
                self.assertFalse(pointer["timedOut"])
                self.assertIsNone(pointer["artifactSha256"])
                event_journal = (
                    self.verify.RUST_COVERAGE_LATEST.parent / pointer["eventJournal"]
                ).resolve()
                self.assertEqual(
                    pointer["eventJournalSha256"],
                    self.supervisor.sha256_file(event_journal),
                )
                terminal = json.loads(
                    event_journal.read_text(encoding="utf-8").splitlines()[-1]
                )
                self.assertEqual(terminal["event"], "phase_end")
                self.assertEqual(terminal["runToken"], pointer["runToken"])
                self.assertEqual(terminal["verdict"], pointer["verdict"])
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "ended FAIL with exit 1"
                ):
                    self.verify._validate_latest_rust_coverage_pointer(
                        self.verify.RUST_COVERAGE_LATEST,
                        expected_sha=pointer["fullGitSha"],
                        expected_checkout_digest=self.verify._checkout_state_digest(),
                    )
                event_journal.write_bytes(event_journal.read_bytes() + b"substitution")
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "journal is missing or changed"
                ):
                    self.verify._validate_failed_rust_coverage_pointer(
                        self.verify.RUST_COVERAGE_LATEST,
                        expected_sha=pointer["fullGitSha"],
                        expected_token=pointer["runToken"],
                    )
            finally:
                (
                    self.verify.RUST_COVERAGE_PHASE_ROOT,
                    self.verify.RUST_COVERAGE_LATEST,
                    self.verify.RUST_COVERAGE_LOCK,
                ) = original

    def test_post_measurement_publication_failure_cannot_leave_running_pointer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = (
                self.verify.RUST_COVERAGE_PHASE_ROOT,
                self.verify.RUST_COVERAGE_LATEST,
                self.verify.RUST_COVERAGE_LOCK,
            )
            try:
                self.verify.RUST_COVERAGE_PHASE_ROOT = root / "phases"
                self.verify.RUST_COVERAGE_LATEST = (
                    root / "latest-rust-coverage-prerequisite.json"
                )
                self.verify.RUST_COVERAGE_LOCK = (
                    root / "rust-coverage-prerequisite.lease.json"
                )
                process = mock.Mock(pid=os.getpid())

                def spawn_with_artifact(command, **_kwargs):
                    artifact = Path(command[command.index("--output") + 1])
                    artifact.write_bytes(b"synthetic coverage")
                    return process, mock.Mock()

                coverage = {
                    "passed": True,
                    "artifactSha256": hashlib.sha256(b"synthetic coverage").hexdigest(),
                }
                with mock.patch.object(
                    self.verify,
                    "_rust_coverage_environment_document",
                    side_effect=self._environment_document_without_toolchain_probe,
                ), mock.patch.object(
                    self.verify,
                    "spawn_isolated",
                    side_effect=spawn_with_artifact,
                ), mock.patch.object(
                    self.verify,
                    "wait_isolated",
                    return_value=(0, False),
                ), mock.patch.object(
                    self.verify,
                    "_rust_coverage_report",
                    return_value=coverage,
                ), mock.patch.object(
                    self.verify,
                    "_validate_rust_coverage_phase",
                    side_effect=self.verify.EvidenceError(
                        "injected manifest validation failure"
                    ),
                ):
                    code = self.verify.rust_coverage_prerequisite_main()

                self.assertEqual(code, 1)
                pointer = json.loads(
                    self.verify.RUST_COVERAGE_LATEST.read_text(encoding="utf-8")
                )
                self.assertEqual(pointer["state"], "FAILED")
                self.assertEqual(pointer["verdict"], "VERIFIER_FAILURE")
                self.assertEqual(pointer["terminalEvent"], "publication_failure")
                self.assertEqual(pointer["exitCode"], 1)
                self.assertEqual(pointer["artifactSha256"], coverage["artifactSha256"])
                event_journal = (
                    self.verify.RUST_COVERAGE_LATEST.parent / pointer["eventJournal"]
                ).resolve()
                events = [
                    json.loads(line)
                    for line in event_journal.read_text(encoding="utf-8").splitlines()
                ]
                self.assertEqual(
                    [event["event"] for event in events[-2:]],
                    ["phase_end", "publication_failure"],
                )
                self.assertEqual(events[-2]["verdict"], "PASS")
                self.assertEqual(events[-1]["verdict"], "VERIFIER_FAILURE")
                artifact = event_journal.parent / self.verify.RUST_COVERAGE_ARTIFACT_NAME
                artifact.write_bytes(b"substituted coverage")
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "LLVM artifact is missing or changed"
                ):
                    self.verify._validate_failed_rust_coverage_pointer(
                        self.verify.RUST_COVERAGE_LATEST,
                        expected_sha=pointer["fullGitSha"],
                        expected_token=pointer["runToken"],
                    )
            finally:
                (
                    self.verify.RUST_COVERAGE_PHASE_ROOT,
                    self.verify.RUST_COVERAGE_LATEST,
                    self.verify.RUST_COVERAGE_LOCK,
                ) = original

    def test_coverage_lease_loser_cannot_overwrite_active_run_pointer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = (
                self.verify.RUST_COVERAGE_PHASE_ROOT,
                self.verify.RUST_COVERAGE_LATEST,
                self.verify.RUST_COVERAGE_LOCK,
            )
            try:
                self.verify.RUST_COVERAGE_PHASE_ROOT = root / "phases"
                self.verify.RUST_COVERAGE_LATEST = (
                    root / "latest-rust-coverage-prerequisite.json"
                )
                self.verify.RUST_COVERAGE_LOCK = (
                    root / "rust-coverage-prerequisite.lease.json"
                )
                sentinel = b'{"state":"RUNNING","runToken":"active-owner"}\n'
                self.verify.RUST_COVERAGE_LATEST.write_bytes(sentinel)

                with mock.patch.object(
                    self.verify,
                    "acquired_lease",
                    side_effect=self.verify.LeaseError("active owner holds the lease"),
                ):
                    code = self.verify.rust_coverage_prerequisite_main()

                self.assertEqual(code, 1)
                self.assertEqual(self.verify.RUST_COVERAGE_LATEST.read_bytes(), sentinel)
            finally:
                (
                    self.verify.RUST_COVERAGE_PHASE_ROOT,
                    self.verify.RUST_COVERAGE_LATEST,
                    self.verify.RUST_COVERAGE_LOCK,
                ) = original

    def test_rust_coverage_prerequisite_rejects_missing_wrong_stale_subthreshold_and_forged_evidence(self) -> None:
        sha = self.verify._full_git_sha()
        checkout_digest = self.verify._checkout_state_digest()

        with tempfile.TemporaryDirectory() as temporary:
            # resolve(): evidence validation compares resolved artifact paths against this root;
            # macOS temp is aliased (/var -> /private/var), so hand it the canonical spelling.
            temporary = os.fspath(Path(temporary).resolve())
            manifest_path, manifest = self._write_coverage_phase(
                Path(temporary) / "valid",
                sha=sha,
                checkout_digest=checkout_digest,
            )
            accepted = self.verify._validate_rust_coverage_phase(
                manifest_path,
                expected_sha=sha,
                expected_checkout_digest=checkout_digest,
                require_fresh=True,
                require_current_environment=False,
            )
            self.assertTrue(accepted["coverage"]["passed"])
            self.assertEqual(
                accepted["coverage"]["toolchain"],
                self.verify._expected_rust_coverage_toolchain_identity(),
            )

            wrong_toolchain = json.loads(json.dumps(manifest))
            wrong_toolchain["environment"]["coverageToolchain"]["toolchain"] = "nightly-2099-01-01"
            self.supervisor.atomic_write_json(manifest_path, wrong_toolchain)
            with self.assertRaisesRegex(self.verify.EvidenceError, "nightly identity"):
                self.verify._validate_rust_coverage_phase(
                    manifest_path,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )
            self.supervisor.atomic_write_json(manifest_path, manifest)

            boolean_schema = json.loads(json.dumps(manifest))
            boolean_schema["schema"] = True
            self.supervisor.atomic_write_json(manifest_path, boolean_schema)
            with self.assertRaisesRegex(self.verify.EvidenceError, "completion/run identity"):
                self.verify._validate_rust_coverage_phase(
                    manifest_path,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )
            self.supervisor.atomic_write_json(manifest_path, manifest)

            with self.assertRaisesRegex(self.verify.EvidenceError, "another source state"):
                self.verify._validate_rust_coverage_phase(
                    manifest_path,
                    expected_sha="f" * 40,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )

            artifact_path = manifest_path.parent / self.verify.RUST_COVERAGE_ARTIFACT_NAME
            original_artifact = artifact_path.read_bytes()
            artifact_path.unlink()
            with self.assertRaisesRegex(self.verify.EvidenceError, "missing|changed"):
                self.verify._validate_rust_coverage_phase(
                    manifest_path,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )
            artifact_path.write_bytes(original_artifact)

            forged = json.loads(json.dumps(manifest))
            forged["coverage"]["artifactSha256"] = "0" * 64
            self.supervisor.atomic_write_json(manifest_path, forged)
            with self.assertRaisesRegex(self.verify.EvidenceError, "forged|substituted"):
                self.verify._validate_rust_coverage_phase(
                    manifest_path,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )

        stale_end = datetime.now(timezone.utc).replace(microsecond=0) - timedelta(
            seconds=self.verify.RUST_COVERAGE_FRESH_SECONDS + 60
        )
        with tempfile.TemporaryDirectory() as temporary:
            stale_path, _ = self._write_coverage_phase(
                Path(temporary) / "stale",
                sha=sha,
                checkout_digest=checkout_digest,
                ended=stale_end,
            )
            with self.assertRaisesRegex(self.verify.EvidenceError, "stale"):
                self.verify._validate_rust_coverage_phase(
                    stale_path,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                    require_current_environment=False,
                )

        for label, payload, message in (
            ("subthreshold", self._llvm_coverage_payload(covered=79), "threshold"),
            ("branchless", self._llvm_coverage_payload(branch_count=0), "zero denominator"),
        ):
            with tempfile.TemporaryDirectory() as temporary:
                # resolve(): _rust_coverage_phase_artifacts inventories resolved paths; an aliased
                # temp root (macOS /var -> /private/var) would fail the inventory before the
                # threshold refusal under test is ever reached.
                temporary = os.fspath(Path(temporary).resolve())
                invalid_path, invalid_manifest = self._write_coverage_phase(
                    Path(temporary) / label,
                    sha=sha,
                    checkout_digest=checkout_digest,
                )
                invalid_artifact = invalid_path.parent / self.verify.RUST_COVERAGE_ARTIFACT_NAME
                invalid_artifact.write_bytes(payload)
                invalid_manifest["artifacts"] = self.verify._rust_coverage_phase_artifacts(
                    invalid_path.parent
                )
                self.supervisor.atomic_write_json(invalid_path, invalid_manifest)
                with self.subTest(label=label), self.assertRaisesRegex(
                    self.verify.EvidenceError, message
                ):
                    self.verify._validate_rust_coverage_phase(
                        invalid_path,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                        require_fresh=False,
                        require_current_environment=False,
                    )

        with tempfile.TemporaryDirectory() as temporary:
            copied_path, _ = self._write_coverage_phase(
                Path(temporary) / "original",
                sha=sha,
                checkout_digest=checkout_digest,
            )
            manual_copy = Path(temporary) / "manual-copy" / copied_path.name
            manual_copy.parent.mkdir()
            shutil.copy2(copied_path, manual_copy)
            with self.assertRaisesRegex(self.verify.EvidenceError, "run identity"):
                self.verify._validate_rust_coverage_phase(
                    manual_copy,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=False,
                    require_current_environment=False,
                )

    def test_product_profiles_cannot_omit_core_gates_or_mandatory_evidence_classes(self) -> None:
        owner_gates = {
            gate.id for gate in self.verify.GATES if self.verify.PROFILE_OWNER in gate.profiles
        }
        windows_gates = {
            gate.id for gate in self.verify.GATES if self.verify.PROFILE_WINDOWS in gate.profiles
        }
        clean_source_profiles = self.verify._gate_by_id("clean-source-tree").profiles
        self.assertEqual(clean_source_profiles, self.verify.PROFILES)
        self.assertTrue(owner_gates <= windows_gates)
        external_review_gates = set(self.verify.EXTERNAL_REVIEW_GATE_IDS)
        self.assertTrue(external_review_gates <= windows_gates)
        self.assertTrue(
            owner_gates.isdisjoint(external_review_gates),
            "owner-product must not depend on remote reviewers or the separately operated pool",
        )
        self.assertIn("branch-protection", windows_gates)
        self.assertNotIn(
            "branch-protection",
            owner_gates,
            "one exact local owner binary must not depend on remote repository administration",
        )
        self.assertTrue(
            {
                "database-integrity-live",
                "review-schema-contract-live",
                "dataset-duplicates",
                "review-serving-provenance",
                "owner-workstation-health-live",
                "real-app-e2e",
                "champion-7b-preflight",
                "durability-drill",
                "export-kill-drill",
            }
            <= owner_gates,
            "owner-product must retain every local data, champion, review, and recovery authority",
        )
        self.assertTrue(
            {
                "database-integrity-live",
                "review-schema-contract-live",
                "review-compensation-readiness",
                "review-mode-certification",
                "playback-enforcement-readiness",
                "real-app-e2e",
                "champion-7b-preflight",
                "durability-drill",
                "export-kill-drill",
            }
            <= windows_gates
        )

        owner_evidence = set(self.verify.PROFILE_REQUIRED_EVIDENCE[self.verify.PROFILE_OWNER])
        windows_evidence = set(self.verify.PROFILE_REQUIRED_EVIDENCE[self.verify.PROFILE_WINDOWS])
        self.assertIn(
            self.verify.PROFILE_OWNER,
            self.verify._gate_by_id("license-compat").profiles,
        )
        self.assertTrue(owner_evidence <= windows_evidence)
        self.assertTrue(
            {
                "architecture-contract",
                "coverage-and-mutation-thresholds",
                "known-defect-ledger",
                "schema-clone-and-restore-campaign",
                "owner-workflow-and-recovery-campaign",
                "owner-field-sessions",
            }
            <= owner_evidence
        )
        self.assertTrue(
            {
                "owner-product-attestation",
                "signed-windows-release-artifacts",
                "supported-windows-vm-campaign",
                "windows-update-rollback-uninstall-campaign",
                "windows-manual-accessibility",
                "windows-comparator-study",
                "windows-five-user-pilot",
            }
            <= windows_evidence
        )
        descoped = {item[0] for item in self.verify.DESCOPED}
        self.assertFalse(
            descoped
            & {"signed-installer", "slsa-provenance", "signed-auto-updater", "signed-tag-protected-main"}
        )
        registry = self.verify.gate_registry_document()
        self.assertEqual(registry["evidenceContract"], self.verify.evidence_contract_document())
        registry_by_id = {gate["id"]: gate for gate in registry["gates"]}
        for gate_id in self.verify.LIVE_AUTHORITY_GATE_IDS:
            self.assertTrue(registry_by_id[gate_id]["liveAuthorityGate"])
            self.assertEqual(
                registry_by_id[gate_id]["diagnosticOverrideAllowlist"],
                list(self.verify.LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT),
            )
            overlap = (
                set(registry_by_id[gate_id]["environmentAllowlist"])
                & set(self.verify.LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
            )
            self.assertEqual(
                overlap,
                (
                    {"CORTEX_APP_EXE"}
                    if gate_id in {"exe-freshness", "playback-enforcement-readiness"}
                    else set()
                ),
            )
        freshness = registry["evidenceContract"]["freshnessPolicy"]
        self.assertEqual(freshness["state"], "PARTIAL_CLASS_SPECIFIC_FAIL_CLOSED")
        self.assertIn("measuredAt", freshness["rule"])
        self.assertIn("expiresAt", freshness["rule"])
        self.assertIn("immutableAuthority", freshness["rule"])
        self.assertIn("PENDING_EXTERNAL", freshness["rule"])
        classes = {item["id"]: item for item in registry["evidenceContract"]["classes"]}
        self.assertEqual(
            classes["timeout-calibration-baselines"]["validatorGate"],
            "timeout-calibration-evidence",
        )
        self.assertEqual(
            classes["verifier-fault-campaigns"]["validatorGate"],
            "verifier-fault-campaign-evidence",
        )
        self.assertEqual(
            classes["architecture-contract"]["validatorGate"],
            "architecture-contract-evidence",
        )
        self.assertEqual(
            classes["known-defect-ledger"]["validatorGate"],
            "known-defect-ledger-evidence",
        )
        for class_id, gate_id in self.verify.OWNER_EVIDENCE_CLASS_GATE_IDS.items():
            self.assertEqual(classes[class_id]["validatorGate"], gate_id)

    def test_evidence_and_release_contracts_fail_closed_on_omission_or_substitution(self) -> None:
        profile = self.verify.PROFILE_WINDOWS
        evidence = self.verify._pending_evidence_results(profile)
        self.assertTrue(evidence)
        self.assertEqual(self.verify._validate_evidence_results(profile, evidence), evidence)

        mutations = [
            evidence[:-1],
            [{**evidence[0], "classId": "substituted"}, *evidence[1:]],
            [{**evidence[0], "status": "VERIFIED"}, *evidence[1:]],
        ]
        for forged in mutations:
            with self.subTest(forged=forged), self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_evidence_results(profile, forged)

        with self.assertRaisesRegex(self.verify.EvidenceError, "omits release-artifact roles"):
            self.verify._validate_release_artifacts(profile, [], "a" * 40, eligible=True)
        substituted = [
            {
                "role": "windows-msi",
                "name": "cortex.msi",
                "sha256": "a" * 64,
                "bytes": 1,
            },
            {
                "role": "windows-msi",
                "name": "other.msi",
                "sha256": "b" * 64,
                "bytes": 1,
            },
        ]
        with self.assertRaisesRegex(self.verify.EvidenceError, "duplicate"):
            self.verify._validate_release_artifacts(
                profile, substituted, "a" * 40, eligible=False
            )
        with self.assertRaisesRegex(self.verify.EvidenceError, "legacy aggregate path is retired"):
            self.verify._retired_aggregate_main(False)

    def test_class_evidence_is_rederived_from_the_exact_validator_artifact(self) -> None:
        sha = "a" * 40
        with tempfile.TemporaryDirectory() as temporary:
            proof_root = Path(temporary)
            relative = Path("gates") / "known-defect-ledger-evidence" / "known-defect-ledger.json"
            artifact_path = proof_root / relative
            artifact_path.parent.mkdir(parents=True)
            artifact = {
                "schema": 1,
                "classId": "known-defect-ledger",
                "fullGitSha": sha,
                "measuredAt": "2026-08-27T00:00:00Z",
                "immutableAuthority": "exact-git-commit",
                "passed": True,
                "failures": [],
                "blockingDefectIds": [],
                "ledger": {},
                "defects": [],
            }
            self.supervisor.atomic_write_json(artifact_path, artifact)
            results = [
                {"gateId": "clean-source-tree", "status": self.verify.PASS},
                {"gateId": "architecture-contract-evidence", "status": self.verify.FAIL},
                {"gateId": "rust-architecture-truth", "status": self.verify.PASS},
                {"gateId": "python-policies", "status": self.verify.PASS},
                {"gateId": "typecheck", "status": self.verify.PASS},
                {"gateId": "lint-js", "status": self.verify.PASS},
                {
                    "gateId": "known-defect-ledger-evidence",
                    "status": self.verify.PASS,
                    "artifacts": [
                        {
                            "path": relative.as_posix(),
                            "sha256": self.supervisor.sha256_file(artifact_path),
                            "bytes": artifact_path.stat().st_size,
                        }
                    ],
                },
            ]
            evidence = self.verify._derive_evidence_results(
                self.verify.PROFILE_OWNER,
                results,
                proof_root,
                sha,
            )
            by_id = {item["classId"]: item for item in evidence}
            self.assertEqual(
                by_id["architecture-contract"]["status"],
                self.verify.EVIDENCE_FAILED,
            )
            self.assertEqual(
                by_id["known-defect-ledger"]["status"],
                self.verify.EVIDENCE_VERIFIED,
            )
            self.assertEqual(
                self.verify._validate_evidence_results(
                    self.verify.PROFILE_OWNER,
                    evidence,
                    results=results,
                    proof_root=proof_root,
                    expected_sha=sha,
                ),
                evidence,
            )

            forged = json.loads(json.dumps(evidence))
            next(
                item for item in forged if item["classId"] == "known-defect-ledger"
            )["evidence"]["sha256"] = "0" * 64
            with self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_evidence_results(
                    self.verify.PROFILE_OWNER,
                    forged,
                    results=results,
                    proof_root=proof_root,
                    expected_sha=sha,
                )

    def test_all_six_owner_evidence_classes_bind_exact_campaign_artifacts_and_fail_closed(self) -> None:
        sha = self.verify._full_git_sha()
        registry_hash = self.verify.gate_registry_hash()
        checkout_digest = self.verify._checkout_state_digest()
        environment = self.verify._environment_document()
        classes = tuple(self.verify.OWNER_EVIDENCE_CLASS_GATE_IDS)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source_root = root / "source"
            gate_roots: dict[str, Path] = {}
            for index, class_id in enumerate(classes, start=1):
                self._write_owner_campaign_shell(
                    source_root,
                    class_id,
                    sha=sha,
                    registry_hash=registry_hash,
                    checkout_digest=checkout_digest,
                    environment=environment,
                    token_suffix=index,
                )
                gate_root = root / f"gate-{index}"
                gate_root.mkdir()
                gate_roots[class_id] = gate_root
                original = self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT, self.verify.LOG_DIR
                try:
                    self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT = source_root
                    self.verify.LOG_DIR = gate_root
                    with mock.patch.object(
                        self.verify,
                        "_validate_owner_campaign_semantics",
                        return_value={"semanticClass": class_id},
                    ), mock.patch.object(
                        self.verify, "_full_git_sha", return_value=sha
                    ), mock.patch.object(
                        self.verify, "gate_registry_hash", return_value=registry_hash
                    ), mock.patch.object(
                        self.verify, "_checkout_state_digest", return_value=checkout_digest
                    ), mock.patch.object(
                        self.verify, "_environment_document", return_value=environment
                    ):
                        report = self.verify._build_owner_class_evidence(
                            class_id,
                            profile=self.verify.PROFILE_OWNER,
                        )
                finally:
                    self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT, self.verify.LOG_DIR = original
                artifact = gate_root / self.verify.OWNER_EVIDENCE_CLASS_ARTIFACTS[class_id]
                self.supervisor.atomic_write_json(artifact, report)
                with mock.patch.object(
                    self.verify,
                    "_validate_owner_campaign_semantics",
                    return_value={"semanticClass": class_id},
                ):
                    self.verify._validate_class_evidence_artifact(
                        class_id,
                        artifact,
                        expected_sha=sha,
                        expected_profile=self.verify.PROFILE_OWNER,
                        expected_registry_hash=registry_hash,
                        expected_checkout_digest=checkout_digest,
                        expected_environment=environment,
                    )
                forged = json.loads(json.dumps(report))
                forged["observations"] = {"semanticClass": "substituted"}
                self.supervisor.atomic_write_json(artifact, forged)
                with mock.patch.object(
                    self.verify,
                    "_validate_owner_campaign_semantics",
                    return_value={"semanticClass": class_id},
                ), self.assertRaises(self.verify.EvidenceError, msg=class_id):
                    self.verify._validate_class_evidence_artifact(
                        class_id,
                        artifact,
                        expected_sha=sha,
                        expected_profile=self.verify.PROFILE_OWNER,
                        expected_registry_hash=registry_hash,
                        expected_checkout_digest=checkout_digest,
                        expected_environment=environment,
                    )

            # Every class must reject a retry/skip/cross-SHA source even if the raw report says pass.
            for index, class_id in enumerate(classes, start=1):
                manifest_path = (
                    source_root
                    / class_id
                    / f"{index:032x}"
                    / self.verify.OWNER_EVIDENCE_SOURCE_MANIFEST
                )
                original_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                for field, value in (
                    ("attemptCount", True),
                    ("retryCount", 1),
                    ("skipCount", 1),
                    ("fullGitSha", "0" * 40),
                ):
                    mutated = json.loads(json.dumps(original_manifest))
                    mutated[field] = value
                    self.supervisor.atomic_write_json(manifest_path, mutated)
                    with self.subTest(class_id=class_id, field=field), mock.patch.object(
                        self.verify,
                        "_validate_owner_campaign_semantics",
                        return_value={"semanticClass": class_id},
                    ), self.assertRaises(self.verify.EvidenceError):
                        self.verify._validate_owner_source_campaign(
                            class_id,
                            manifest_path,
                            expected_sha=sha,
                            expected_registry_hash=registry_hash,
                            expected_checkout_digest=checkout_digest,
                            expected_environment=environment,
                            require_fresh=True,
                        )
                self.supervisor.atomic_write_json(manifest_path, original_manifest)

            duplicate_path = (
                source_root
                / classes[0]
                / f"{1:032x}"
                / self.verify.OWNER_EVIDENCE_SOURCE_MANIFEST
            )
            duplicate_path.write_text('{"schema":1,"schema":1}\n', encoding="utf-8")
            with self.assertRaisesRegex(self.verify.EvidenceError, "duplicate JSON key"):
                self.verify._load_json_without_duplicate_keys(duplicate_path)

            future_manifest = (
                source_root
                / classes[1]
                / f"{2:032x}"
                / self.verify.OWNER_EVIDENCE_SOURCE_MANIFEST
            )
            future = json.loads(future_manifest.read_text(encoding="utf-8"))
            future_start = datetime.now(timezone.utc).replace(microsecond=0) + timedelta(hours=1)
            future_end = future_start + timedelta(minutes=1)
            future["startedAt"] = self.verify._format_utc(future_start)
            future["endedAt"] = self.verify._format_utc(future_end)
            future["expiresAt"] = self.verify._format_utc(
                future_end
                + timedelta(
                    seconds=self.verify._owner_campaign_fresh_seconds(classes[1])
                )
            )
            self.supervisor.atomic_write_json(future_manifest, future)
            with self.assertRaisesRegex(self.verify.EvidenceError, "stale"):
                self.verify._validate_owner_source_campaign(
                    classes[1],
                    future_manifest,
                    expected_sha=sha,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                    require_fresh=True,
                )

    def test_terminal_frontend_coverage_replay_rejects_timeout_nonzero_and_malformed_output(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            frontend_root = Path(temporary) / "frontend"
            frontend_root.mkdir()
            manifest_path = frontend_root / "frontend-coverage-raw-manifest.json"
            bundle_path = frontend_root / "frontend-coverage-raw.v1.bin"
            manifest_path.write_text("{}\n", encoding="utf-8")
            bundle_path.write_bytes(b"CORTEX_FRONTEND_COVERAGE_RAW_V1\nfixture")
            metric = {"total": 10, "covered": 10, "skipped": 0, "pct": 100.0}
            summary = {
                name: dict(metric) for name in ("lines", "statements", "branches", "functions")
            }
            critical = {
                domain: {name: dict(metric) for name in summary}
                for domain in ("audio-state-machine", "review-truth-reducers")
            }
            run_token = "01234567-89ab-4cde-8fab-0123456789ab"
            evidence = {
                "runToken": run_token,
                "fullE2ETests": 119,
                "instrumentedE2ETests": 110,
                "e2eRawFiles": 110,
                "e2eConvertedSourceFiles": 12,
            }
            replay = {
                "schema": 1,
                "type": "FrontendCoverageReplayV1",
                "certificationEligible": True,
                "runToken": run_token,
                "sourceTreeSha256": "a" * 64,
                "campaignInputsSha256": "b" * 64,
                "manifestSha256": self.supervisor.sha256_file(manifest_path),
                "bundleSha256": self.supervisor.sha256_file(bundle_path),
                "fullE2ETests": 119,
                "instrumentedE2ETests": 110,
                "e2eRawFiles": 110,
                "e2eConvertedSourceFiles": 12,
                "summary": summary,
                "criticalDomains": critical,
            }
            paths = {
                "frontend/frontend-coverage-raw-manifest.json": manifest_path,
                "frontend/frontend-coverage-raw.v1.bin": bundle_path,
            }

            def completed(
                value: object = replay,
                *,
                returncode: int = 0,
                stderr: str = "",
                stdout: str | None = None,
            ) -> subprocess.CompletedProcess[str]:
                rendered = (
                    json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
                    + "\n"
                    if stdout is None
                    else stdout
                )
                return subprocess.CompletedProcess([], returncode, rendered, stderr)

            with mock.patch.object(
                self.verify.subprocess,
                "run",
                return_value=completed(),
            ) as run:
                observed = self.verify._run_frontend_coverage_replay(
                    paths,
                    evidence=evidence,
                    source_digest="a" * 64,
                    campaign_digest="b" * 64,
                    summary=summary,
                    critical_summaries=critical,
                )
            self.assertEqual(observed, replay)
            self.assertEqual(run.call_args.kwargs["timeout"], 600)
            self.assertFalse(run.call_args.kwargs["shell"])
            argv = run.call_args.args[0]
            self.assertEqual(argv[2:4], ["--replay", "--manifest"])
            self.assertEqual(Path(argv[4]), manifest_path.resolve())
            self.assertEqual(argv[5], "--bundle")
            self.assertEqual(Path(argv[6]), bundle_path.resolve())
            self.assertEqual(argv[7], "--temporary-parent")

            original_bundle = bundle_path.read_bytes()

            def replace_frontend_authority_during_replay(
                *_args: object, **_kwargs: object
            ) -> subprocess.CompletedProcess[str]:
                bundle_path.write_bytes(original_bundle + b"swapped")
                return completed(
                    {
                        **replay,
                        "bundleSha256": self.supervisor.sha256_file(bundle_path),
                    }
                )

            try:
                with mock.patch.object(
                    self.verify.subprocess,
                    "run",
                    side_effect=replace_frontend_authority_during_replay,
                ), self.assertRaisesRegex(self.verify.EvidenceError, "changed during replay"):
                    self.verify._run_frontend_coverage_replay(
                        paths,
                        evidence=evidence,
                        source_digest="a" * 64,
                        campaign_digest="b" * 64,
                        summary=summary,
                        critical_summaries=critical,
                    )
            finally:
                bundle_path.write_bytes(original_bundle)

            forged_summary = json.loads(json.dumps(replay))
            forged_summary["summary"]["lines"]["covered"] = 9
            attacks = {
                "nonzero": completed(returncode=1),
                "stderr": completed(stderr="warning"),
                "malformed-json": completed(stdout="not-json\n"),
                "duplicate-key": completed(stdout='{"schema":1,"schema":1}\n'),
                "noncanonical-json": completed(stdout=json.dumps(replay) + "\n"),
                "noncertifying": completed({**replay, "certificationEligible": False}),
                "wrong-bundle-hash": completed({**replay, "bundleSha256": "0" * 64}),
                "boolean-count": completed({**replay, "e2eRawFiles": True}),
                "forged-summary": completed(forged_summary),
            }
            for label, result in attacks.items():
                with self.subTest(label=label), mock.patch.object(
                    self.verify.subprocess,
                    "run",
                    return_value=result,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._run_frontend_coverage_replay(
                        paths,
                        evidence=evidence,
                        source_digest="a" * 64,
                        campaign_digest="b" * 64,
                        summary=summary,
                        critical_summaries=critical,
                    )
            with mock.patch.object(
                self.verify.subprocess,
                "run",
                side_effect=subprocess.TimeoutExpired(["node"], 600),
            ), self.assertRaisesRegex(self.verify.EvidenceError, "explicit timeout"):
                self.verify._run_frontend_coverage_replay(
                    paths,
                    evidence=evidence,
                    source_digest="a" * 64,
                    campaign_digest="b" * 64,
                    summary=summary,
                    critical_summaries=critical,
                )

    def test_terminal_mutation_replay_rejects_timeout_nonzero_and_malformed_output(self) -> None:
        sha = self.verify._full_git_sha()
        checkout_digest = self.verify._checkout_state_digest()
        with tempfile.TemporaryDirectory() as temporary:
            mutation_root = Path(temporary) / "mutation"
            mutation_root.mkdir()
            manifest_path = mutation_root / "owner-mutation-raw-manifest.json"
            bundle_path = mutation_root / "owner-mutation-raw.v1.bin"
            bundle_path.write_bytes(b"CORTEX_OWNER_MUTATION_RAW_V1\nfixture")
            backend_domains = {
                domain: {"mutants": 1, "killed": 1, "scorePercent": 100.0}
                for domain in self.verify._rust_quality_module().CRITICAL_COVERAGE_DOMAINS
            }
            observations = {
                "backend": {
                    "mutants": len(backend_domains),
                    "killed": len(backend_domains),
                    "domains": backend_domains,
                },
                "frontend": {
                    "mutants": 2,
                    "killed": 2,
                    "domains": {
                        "audio-state-machine": {
                            "mutants": 1,
                            "killed": 1,
                            "scorePercent": 100.0,
                        },
                        "review-truth-reducers": {
                            "mutants": 1,
                            "killed": 1,
                            "scorePercent": 100.0,
                        },
                    },
                },
            }
            raw_manifest = {
                "schema": 1,
                "type": "OwnerMutationRawAuthorityV1",
                "runToken": "01234567-89ab-4cde-8fab-0123456789ab",
                "scope": ["backend", "frontend"],
                "certificationEligible": True,
                "fullGitSha": sha,
                "checkoutStateDigest": checkout_digest,
                "contractSha256": "a" * 64,
                "campaignSha256": "b" * 64,
                "authorities": {},
                "tools": {
                    "cargoMutants": "27.1.0",
                    "stryker": "10.0.0",
                    "strykerVitestRunner": "10.0.0",
                    "vitest": "4.1.10",
                },
                "runtime": {},
                "bundle": {
                    "format": "CORTEX_OWNER_MUTATION_RAW_V1",
                    "sha256": self.supervisor.sha256_file(bundle_path),
                    "bytes": bundle_path.stat().st_size,
                    "entries": [],
                },
            }
            self.supervisor.atomic_write_json(manifest_path, raw_manifest)
            replay = {
                "fullGitSha": sha,
                "scope": ["backend", "frontend"],
                "certificationEligible": True,
                "observations": observations,
                "manifestSha256": self.supervisor.sha256_file(manifest_path),
                "bundleSha256": self.supervisor.sha256_file(bundle_path),
            }
            paths = {
                "mutation/owner-mutation-raw-manifest.json": manifest_path,
                "mutation/owner-mutation-raw.v1.bin": bundle_path,
            }

            def completed(
                value: object = replay,
                *,
                returncode: int = 0,
                stderr: str = "",
                stdout: str | None = None,
            ) -> subprocess.CompletedProcess[str]:
                rendered = (
                    json.dumps(value, ensure_ascii=False, sort_keys=True) + "\n"
                    if stdout is None
                    else stdout
                )
                return subprocess.CompletedProcess([], returncode, rendered, stderr)

            with mock.patch.object(
                self.verify.subprocess,
                "run",
                return_value=completed(),
            ) as run:
                observed, observed_manifest = self.verify._run_owner_mutation_replay(
                    paths,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                )
            self.assertEqual(observed, replay)
            self.assertEqual(observed_manifest, raw_manifest)
            self.assertEqual(run.call_args.kwargs["timeout"], 600)
            self.assertFalse(run.call_args.kwargs["shell"])
            self.assertEqual(run.call_args.args[0][-1], "--replay")
            self.assertEqual(run.call_args.args[0][-3], "--output")
            self.assertEqual(Path(run.call_args.args[0][-2]), mutation_root.resolve())

            original_manifest = manifest_path.read_bytes()

            def replace_mutation_authority_during_replay(
                *_args: object, **_kwargs: object
            ) -> subprocess.CompletedProcess[str]:
                manifest_path.write_bytes(original_manifest + b" ")
                return completed(
                    {
                        **replay,
                        "manifestSha256": self.supervisor.sha256_file(manifest_path),
                    }
                )

            try:
                with mock.patch.object(
                    self.verify.subprocess,
                    "run",
                    side_effect=replace_mutation_authority_during_replay,
                ), self.assertRaisesRegex(self.verify.EvidenceError, "changed during replay"):
                    self.verify._run_owner_mutation_replay(
                        paths,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                    )
            finally:
                manifest_path.write_bytes(original_manifest)

            attacks = {
                "nonzero": completed(returncode=1),
                "stderr": completed(stderr="warning"),
                "malformed-json": completed(stdout="not-json\n"),
                "duplicate-key": completed(
                    stdout='{"scope":[],"scope":["backend","frontend"]}\n'
                ),
                "noncanonical-json": completed(stdout=json.dumps(replay) + "  \n"),
                "partial-scope": completed({**replay, "scope": ["frontend"]}),
                "noncertifying": completed({**replay, "certificationEligible": False}),
                "wrong-manifest-hash": completed({**replay, "manifestSha256": "0" * 64}),
                "malformed-observation": completed(
                    {
                        **replay,
                        "observations": {
                            **observations,
                            "frontend": {
                                **observations["frontend"],
                                "mutants": True,
                            },
                        },
                    }
                ),
            }
            for label, result in attacks.items():
                with self.subTest(label=label), mock.patch.object(
                    self.verify.subprocess,
                    "run",
                    return_value=result,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._run_owner_mutation_replay(
                        paths,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                    )
            with mock.patch.object(
                self.verify.subprocess,
                "run",
                side_effect=subprocess.TimeoutExpired(["python"], 600),
            ), self.assertRaisesRegex(self.verify.EvidenceError, "explicit timeout"):
                self.verify._run_owner_mutation_replay(
                    paths,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                )

            partial_manifest = json.loads(json.dumps(raw_manifest))
            partial_manifest["scope"] = ["frontend"]
            partial_manifest["certificationEligible"] = False
            self.supervisor.atomic_write_json(manifest_path, partial_manifest)
            with mock.patch.object(
                self.verify.subprocess,
                "run",
                return_value=completed(),
            ) as run, self.assertRaisesRegex(self.verify.EvidenceError, "partial-scope"):
                self.verify._run_owner_mutation_replay(
                    paths,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                )
            run.assert_not_called()

    def test_coverage_mutation_semantics_count_every_non_killed_outcome_against_locked_floors(self) -> None:
        sha = self.verify._full_git_sha()
        checkout_digest = self.verify._checkout_state_digest()
        now = datetime.now(timezone.utc).replace(microsecond=0)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def write_report(path: Path, *, backend: bool) -> dict[str, object]:
                mutants = []
                if backend:
                    quality = self.verify._rust_quality_module()
                    for index, (domain, patterns) in enumerate(
                        quality.CRITICAL_COVERAGE_DOMAINS.items(), start=1
                    ):
                        candidates = []
                        for pattern in patterns:
                            candidates.extend((REPO_ROOT / "cortex-speech-app").glob(pattern))
                        source = next(path for path in candidates if path.is_file())
                        mutants.append(
                            {
                                "id": f"backend-{index}",
                                "domain": domain,
                                "sourcePath": source.relative_to(REPO_ROOT).as_posix(),
                                "outcome": "KILLED",
                            }
                        )
                else:
                    for index, (domain, source_name) in enumerate(
                        (
                            ("audio-state-machine", "audioMachine.ts"),
                            ("review-truth-reducers", "reviewCommitOperation.ts"),
                        ),
                        start=1,
                    ):
                        mutants.append(
                            {
                                "id": f"frontend-{index}",
                                "domain": domain,
                                "sourcePath": (
                                    Path("cortex-speech-app") / "src" / "lib" / source_name
                                ).as_posix(),
                                "outcome": "KILLED",
                            }
                        )
                report = {
                    "schema": 1,
                    "type": (
                        "BackendCriticalMutationReportV1"
                        if backend
                        else "FrontendReducerMutationReportV1"
                    ),
                    "fullGitSha": sha,
                    "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                    "checkoutStateDigest": checkout_digest,
                    "startedAt": self.verify._format_utc(now - timedelta(minutes=2)),
                    "endedAt": self.verify._format_utc(now - timedelta(minutes=1)),
                    "expiresAt": self.verify._format_utc(
                        now
                        - timedelta(minutes=1)
                        + timedelta(seconds=self.verify.OWNER_EVIDENCE_FRESH_SECONDS)
                    ),
                    "attemptCount": 1,
                    "retryCount": 0,
                    "skipCount": 0,
                    "tool": {
                        "name": "cargo-mutants" if backend else "frontend-mutation-runner",
                        "version": "1.0.0-test",
                        "commandRegistrySha256": "c" * 64,
                    },
                    "mutants": mutants,
                }
                self.supervisor.atomic_write_json(path, report)
                return report

            backend_path = root / "backend.json"
            frontend_path = root / "frontend.json"
            backend = write_report(backend_path, backend=True)
            frontend = write_report(frontend_path, backend=False)
            for path, is_backend in ((backend_path, True), (frontend_path, False)):
                with self.assertRaisesRegex(
                    self.verify.EvidenceError,
                    self.verify.UNSUPPORTED_UNBACKED_EVIDENCE,
                ):
                    self.verify._validate_mutation_report(
                        path,
                        backend=is_backend,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                        require_fresh=True,
                    )
            wrong_domain = json.loads(json.dumps(frontend))
            wrong_domain["mutants"][0]["sourcePath"] = (
                Path("cortex-speech-app") / "src" / "lib" / "reviewCommitOperation.ts"
            ).as_posix()
            self.supervisor.atomic_write_json(frontend_path, wrong_domain)
            with self.assertRaisesRegex(self.verify.EvidenceError, "critical reducer domain"):
                self.verify._validate_mutation_report(
                    frontend_path,
                    backend=False,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                )
            for label, original, path, is_backend in (
                ("survivor", backend, backend_path, True),
                ("timeout", frontend, frontend_path, False),
                ("build-error", backend, backend_path, True),
                ("unexplained-exclusion", frontend, frontend_path, False),
            ):
                forged = json.loads(json.dumps(original))
                forged["mutants"][0]["outcome"] = {
                    "survivor": "SURVIVED",
                    "timeout": "TIMEOUT",
                    "build-error": "BUILD_ERROR",
                    "unexplained-exclusion": "EXCLUDED_UNEXPLAINED",
                }[label]
                self.supervisor.atomic_write_json(path, forged)
                with self.subTest(label=label), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_mutation_report(
                        path,
                        backend=is_backend,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                        require_fresh=True,
                    )
            relaxed = json.loads(json.dumps(frontend))
            relaxed["thresholdPercent"] = 0
            self.supervisor.atomic_write_json(frontend_path, relaxed)
            with self.assertRaisesRegex(self.verify.EvidenceError, "non-canonical envelope"):
                self.verify._validate_mutation_report(
                    frontend_path,
                    backend=False,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                )
            boolean_counter = json.loads(json.dumps(frontend))
            boolean_counter["attemptCount"] = True
            self.supervisor.atomic_write_json(frontend_path, boolean_counter)
            with self.assertRaisesRegex(self.verify.EvidenceError, "retried"):
                self.verify._validate_mutation_report(
                    frontend_path,
                    backend=False,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                )

            raw_manifest = {
                "campaignSha256": "d" * 64,
                "tools": {"cargoMutants": "27.1.0", "stryker": "10.0.0"},
            }
            observations = {}
            for label, report in (("backend", backend), ("frontend", frontend)):
                by_domain: dict[str, list[dict[str, object]]] = {}
                for mutant in report["mutants"]:
                    by_domain.setdefault(str(mutant["domain"]), []).append(mutant)
                observations[label] = {
                    "mutants": len(report["mutants"]),
                    "killed": sum(
                        1 for mutant in report["mutants"] if mutant["outcome"] == "KILLED"
                    ),
                    "domains": {
                        domain: {
                            "mutants": len(rows),
                            "killed": sum(1 for row in rows if row["outcome"] == "KILLED"),
                            "scorePercent": sum(
                                1 for row in rows if row["outcome"] == "KILLED"
                            )
                            * 100.0
                            / len(rows),
                        }
                        for domain, rows in by_domain.items()
                    },
                }
            replay = {
                "manifestSha256": "a" * 64,
                "bundleSha256": "b" * 64,
                "observations": observations,
            }
            terminal_reports = []
            for report, backend_report, path, label in (
                (backend, True, backend_path, "backend"),
                (frontend, False, frontend_path, "frontend"),
            ):
                terminal = json.loads(json.dumps(report))
                terminal["tool"]["version"] = (
                    raw_manifest["tools"]["cargoMutants"]
                    if backend_report
                    else raw_manifest["tools"]["stryker"]
                )
                terminal["tool"]["commandRegistrySha256"] = raw_manifest[
                    "campaignSha256"
                ]
                terminal["rawAuthorityManifestSha256"] = replay["manifestSha256"]
                terminal["rawAuthorityBundleSha256"] = replay["bundleSha256"]
                terminal["observation"] = observations[label]
                terminal_reports.append((terminal, backend_report, path))
                self.supervisor.atomic_write_json(path, terminal)
                validated = self.verify._validate_mutation_report(
                    path,
                    backend=backend_report,
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                    replay=replay,
                    raw_manifest=raw_manifest,
                )
                self.assertEqual(validated["mutants"], observations[label]["mutants"])
                self.assertEqual(
                    validated["rawAuthorityManifestSha256"], replay["manifestSha256"]
                )

            for label, terminal, backend_report, path in (
                ("manifest-hash", *terminal_reports[0]),
                ("observation", *terminal_reports[1]),
                ("tool-version", *terminal_reports[0]),
            ):
                forged = json.loads(json.dumps(terminal))
                if label == "manifest-hash":
                    forged["rawAuthorityManifestSha256"] = "0" * 64
                elif label == "observation":
                    forged["observation"]["killed"] -= 1
                else:
                    forged["tool"]["version"] = "substituted"
                self.supervisor.atomic_write_json(path, forged)
                with self.subTest(label=label), self.assertRaisesRegex(
                    self.verify.EvidenceError, "exact projection"
                ):
                    self.verify._validate_mutation_report(
                        path,
                        backend=backend_report,
                        expected_sha=sha,
                        expected_checkout_digest=checkout_digest,
                        require_fresh=True,
                        replay=replay,
                        raw_manifest=raw_manifest,
                    )

    def test_istanbul_recomputation_accepts_only_the_real_canonical_metadata_shape(self) -> None:
        source = (REPO_ROOT / "cortex-speech-app" / "src" / "lib" / "audioMachine.ts").resolve()
        row = {
            "path": str(source),
            "statementMap": {"0": {"start": {"line": 1}}},
            "fnMap": {"0": {}},
            "branchMap": {"0": {"locations": [{}, {}]}},
            "s": {"0": 1},
            "f": {"0": 0},
            "b": {"0": [1, 0]},
            "meta": {
                "lastBranch": 1,
                "lastFunction": 1,
                "lastStatement": 1,
                "seen": {},
                "fnNames": {},
            },
        }
        summary, observed = self.verify._istanbul_coverage_summary({str(source): row})
        self.assertEqual(observed, {"lib/audiomachine.ts"})
        self.assertEqual(summary["statements"]["pct"], 100.0)
        self.assertEqual(summary["functions"]["pct"], 0.0)
        self.assertEqual(summary["branches"]["pct"], 50.0)
        forged = json.loads(json.dumps(row))
        forged["meta"]["unbound"] = True
        with self.assertRaisesRegex(self.verify.EvidenceError, "metadata"):
            self.verify._istanbul_coverage_summary({str(source): forged})
        alias = str(source.parent / ".." / source.parent.name / source.name)
        aliased = json.loads(json.dumps(row))
        aliased["path"] = alias
        with self.assertRaisesRegex(self.verify.EvidenceError, "alias-free"):
            self.verify._istanbul_coverage_summary({alias: aliased})

    def test_fabricated_all_covered_istanbul_map_cannot_certify_without_raw_runner_authority(self) -> None:
        """A complete-looking Istanbul projection is not evidence that its counters were observed."""

        shipped = self.verify._frontend_shipped_sources()
        forged_map: dict[str, object] = {}
        for source in shipped:
            absolute = str(source.resolve())
            forged_map[absolute] = {
                "path": absolute,
                "statementMap": {"0": {"start": {"line": 1}}},
                "fnMap": {"0": {}},
                "branchMap": {"0": {"locations": [{}, {}]}},
                "s": {"0": 1},
                "f": {"0": 1},
                "b": {"0": [1, 1]},
            }
        summary, observed = self.verify._istanbul_coverage_summary(forged_map)
        self.assertEqual(
            observed,
            {
                path.resolve().relative_to((REPO_ROOT / "cortex-speech-app" / "src").resolve())
                .as_posix()
                .casefold()
                for path in shipped
            },
        )
        self.assertTrue(all(row["pct"] == 100.0 for row in summary.values()))
        forged_critical: dict[str, object] = {}
        for domain, relative_sources in {
            "audio-state-machine": ["src/lib/audioMachine.ts"],
            "review-truth-reducers": [
                "src/lib/reviewCommitOperation.ts",
                "src/lib/reviewCommitResult.ts",
            ],
        }.items():
            domain_map = {
                str((REPO_ROOT / "cortex-speech-app" / relative).resolve()): forged_map[
                    str((REPO_ROOT / "cortex-speech-app" / relative).resolve())
                ]
                for relative in relative_sources
            }
            domain_summary, _domain_observed = self.verify._istanbul_coverage_summary(
                domain_map
            )
            forged_critical[domain] = domain_summary

        sha = self.verify._full_git_sha()
        checkout_digest = self.verify._checkout_state_digest()
        now = datetime.now(timezone.utc).replace(microsecond=0)
        expires = now + timedelta(days=1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            contract_path = root / "frontend-coverage-contract.v1.json"
            shutil.copy2(
                REPO_ROOT
                / "cortex-speech-app"
                / "scripts"
                / "frontend_coverage_contract.v1.json",
                contract_path,
            )
            coverage_path = root / "coverage-final.json"
            summary_path = root / "coverage-summary.json"
            evidence_path = root / "frontend-coverage-evidence.json"
            raw_manifest_path = root / "frontend-coverage-raw-manifest.json"
            raw_bundle_path = root / "frontend-coverage-raw.v1.bin"
            self.supervisor.atomic_write_json(coverage_path, forged_map)
            raw_bundle_path.write_bytes(b"CORTEX_FRONTEND_COVERAGE_RAW_V1\n" + b"x")
            self.supervisor.atomic_write_json(
                summary_path,
                {
                    "total": {
                        **summary,
                        "branchesTrue": {
                            "total": 0,
                            "covered": 0,
                            "skipped": 0,
                            "pct": 100,
                        },
                    }
                },
            )
            source_rows, source_digest = self.verify._frontend_snapshot(shipped)
            campaign_rows, campaign_digest = self.verify._frontend_snapshot(
                self.verify._frontend_campaign_inputs()
            )
            run_token = "01234567-89ab-4cde-8fab-0123456789ab"
            authority_paths = {
                "contract": "scripts/frontend_coverage_contract.v1.json",
                "runner": "scripts/run_merged_frontend_coverage.mjs",
                "packageLock": "package-lock.json",
                "vitestConfig": "vitest.config.ts",
                "playwrightConfig": "playwright.config.ts",
            }
            self.supervisor.atomic_write_json(
                raw_manifest_path,
                {
                    "schema": 1,
                    "type": "FrontendCoverageRawAuthorityV1",
                    "runToken": run_token,
                    "sourceTree": {"entries": source_rows, "sha256": source_digest},
                    "campaignInputs": {
                        "entries": campaign_rows,
                        "sha256": campaign_digest,
                    },
                    "authorities": {
                        role: {
                            "path": relative,
                            "sha256": self.supervisor.sha256_file(
                                REPO_ROOT / "cortex-speech-app" / Path(relative)
                            ),
                        }
                        for role, relative in authority_paths.items()
                    },
                    "runtime": {
                        "node": "v-test",
                        "platform": "test",
                        "architecture": "x64",
                    },
                    "commands": [
                        {
                            "argv": ["node", "vitest", "run", "--coverage"],
                            "cwd": ".",
                            "environment": {"CORTEX_MERGED_COVERAGE": "1"},
                            "logPath": "raw-authority/unit.log",
                            "status": 0,
                            "signal": None,
                        },
                        {
                            "argv": [
                                "node",
                                "playwright",
                                "test",
                                "--project=chromium",
                                "--workers=1",
                                "--retries=0",
                                "--reporter=line,json",
                            ],
                            "cwd": ".",
                            "environment": {"CORTEX_E2E_COVERAGE": "1"},
                            "logPath": "raw-authority/playwright.log",
                            "status": 0,
                            "signal": None,
                        },
                    ],
                    "bundle": {
                        "format": "CORTEX_FRONTEND_COVERAGE_RAW_V1",
                        "sha256": self.supervisor.sha256_file(raw_bundle_path),
                        "bytes": raw_bundle_path.stat().st_size,
                        "entries": [
                            {
                                "bytes": 1,
                                "path": "logs/unit.log",
                                "sha256": hashlib.sha256(b"x").hexdigest(),
                            }
                        ],
                    },
                },
            )
            self.supervisor.atomic_write_json(
                evidence_path,
                {
                    "schema": 1,
                    "runToken": run_token,
                    "contractSha256": self.supervisor.sha256_file(contract_path),
                    "sourceTreeSha256": source_digest,
                    "campaignInputsSha256": campaign_digest,
                    # These four hashes were previously accepted without the named raw files.
                    "unitCoverageSha256": "1" * 64,
                    "playwrightReportSha256": "2" * 64,
                    "rawCoverageSha256": "3" * 64,
                    "browserCoverageSha256": "4" * 64,
                    "mergedCoverageSha256": self.supervisor.sha256_file(coverage_path),
                    "rawAuthorityManifestSha256": self.supervisor.sha256_file(
                        raw_manifest_path
                    ),
                    "rawAuthorityBundleSha256": self.supervisor.sha256_file(raw_bundle_path),
                    "shippedSourceFiles": len(shipped),
                    "fullE2ETests": 100,
                    "instrumentedE2ETests": 75,
                    "e2eRawFiles": 75,
                    "e2eConvertedSourceFiles": 10,
                    "semanticMapMatch": {
                        metric: {
                            "incomingItems": 1,
                            "matchedItems": 1,
                            "unmatchedItems": 0,
                            "pct": 100.0,
                        }
                        for metric in ("statements", "functions", "branches")
                    },
                    "summary": summary,
                    "criticalDomains": forged_critical,
                },
            )
            paths = {
                "rust/rust-coverage-manifest.json": root / "unused-rust.json",
                "frontend/frontend-coverage-contract.v1.json": contract_path,
                "frontend/frontend-coverage-evidence.json": evidence_path,
                "frontend/frontend-coverage-raw-manifest.json": raw_manifest_path,
                "frontend/frontend-coverage-raw.v1.bin": raw_bundle_path,
                "frontend/coverage-final.json": coverage_path,
                "frontend/coverage-summary.json": summary_path,
                "mutation/backend-mutation.json": root / "unused-backend.json",
                "mutation/frontend-mutation.json": root / "unused-frontend.json",
            }
            prerequisite = {
                "coverage": {"passed": True},
                "expiresAt": self.verify._format_utc(expires),
            }
            mutation = {"expiresAt": self.verify._format_utc(expires)}
            original_contract = contract_path.read_bytes()
            boolean_contract = json.loads(original_contract.decode("utf-8"))
            boolean_contract["schema"] = True
            self.supervisor.atomic_write_json(contract_path, boolean_contract)
            boolean_evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
            boolean_evidence["contractSha256"] = self.supervisor.sha256_file(contract_path)
            self.supervisor.atomic_write_json(evidence_path, boolean_evidence)
            with mock.patch.object(
                self.verify,
                "_validate_rust_coverage_phase",
                return_value=prerequisite,
            ), self.assertRaisesRegex(self.verify.EvidenceError, "contract is substituted"):
                self.verify._validate_coverage_mutation_semantics(
                    paths,
                    manifest={"expiresAt": self.verify._format_utc(expires)},
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                )
            contract_path.write_bytes(original_contract)
            boolean_evidence["contractSha256"] = self.supervisor.sha256_file(contract_path)
            self.supervisor.atomic_write_json(evidence_path, boolean_evidence)
            with mock.patch.object(
                self.verify,
                "_validate_rust_coverage_phase",
                return_value=prerequisite,
            ), mock.patch.object(
                self.verify,
                "_validate_mutation_report",
                return_value=mutation,
            ), mock.patch.object(
                self.verify,
                "_run_owner_mutation_replay",
                return_value=(
                    {
                        "manifestSha256": "a" * 64,
                        "bundleSha256": "b" * 64,
                        "observations": {"backend": {}, "frontend": {}},
                    },
                    {},
                ),
            ), mock.patch.object(
                self.verify,
                "_run_frontend_coverage_replay",
                side_effect=self.verify.EvidenceError(
                    self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
                ),
            ), self.assertRaisesRegex(
                self.verify.EvidenceError,
                self.verify.UNSUPPORTED_UNBACKED_EVIDENCE,
            ):
                self.verify._validate_coverage_mutation_semantics(
                    paths,
                    manifest={"expiresAt": self.verify._format_utc(expires)},
                    expected_sha=sha,
                    expected_checkout_digest=checkout_digest,
                    require_fresh=True,
                )

    def test_owner_proof_manifest_hash_claims_do_not_substitute_for_the_bound_files(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            paths = self._write_owner_proof_fixture(Path(temporary) / "owner-proof", sha)
            with self.assertRaisesRegex(
                self.verify.EvidenceError,
                "file-role inventory|absent or differs|substituted",
            ):
                self.verify._validate_owner_proof_binding(paths, expected_sha=sha)

    def test_product_attestation_rejects_boolean_integer_substitution(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "manifest.json"
            manifest_path.write_text("{}\n", encoding="utf-8")
            manifest = {
                "runToken": "a" * 32,
                "profile": self.verify.PROFILE_OWNER,
                "fullGitSha": "b" * 40,
                "sourceTreeDigest": "c" * 64,
                "checkoutStateDigest": "d" * 64,
                "gateRegistryHash": "e" * 64,
                "evidenceContractHash": "f" * 64,
                "environment": {},
                "runAuthority": {},
                "schemaAuthority": {},
                "releaseArtifacts": [],
                "windowsReleaseAuthority": None,
                "rustCoveragePrerequisite": None,
                "knownDefectDigest": "1" * 64,
                "modelAttestation": None,
                "staleTakeover": {"occurred": False, "abandonedRunToken": None},
                "certificationEligible": False,
                "verdict": "INCOMPLETE",
            }
            attestation = self.verify._product_attestation_document(
                manifest_path, manifest
            )
            attestation["schema"] = True
            attestation_path = root / self.verify.PRODUCT_ATTESTATION_NAME
            self.supervisor.atomic_write_json(attestation_path, attestation)
            with self.assertRaisesRegex(self.verify.EvidenceError, "substituted"):
                self.verify._validate_product_attestation(
                    attestation_path, manifest_path, manifest
                )

    def test_schema_clone_restore_semantics_reject_omission_future_acceptance_and_same_volume(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self._write_owner_proof_fixture(root / "owner-proof", sha)
            truth = "d" * 64
            schema_pairs = {
                "fresh-schema69-install": (0, 69),
                "schema65-to69-live-sized-clone": (65, 69),
                "schema69-reopen": (69, 69),
                "interrupted-migration-recovery": (65, 69),
                "future-schema-refusal": (70, 70),
                "local-snapshot-isolated-restore": (69, 69),
                "offsite-snapshot-isolated-restore": (69, 69),
            }
            segment_count = 43_774
            phases = []
            for index, phase_id in enumerate(self.verify.SCHEMA_CAMPAIGN_PHASES, start=1):
                before, after = schema_pairs[phase_id]
                phases.append(
                    {
                        "id": phase_id,
                        "status": "PASS",
                        "attemptCount": 1,
                        "retryCount": 0,
                        "skipCount": 0,
                        "schemaBefore": before,
                        "schemaAfter": after,
                        "quickCheck": "ok",
                        "integrityCheck": "ok",
                        "foreignKeyViolations": 0,
                        "segmentCount": 0 if phase_id == "fresh-schema69-install" else segment_count,
                        "truthDigest": (
                            hashlib.sha256(b"").hexdigest()
                            if phase_id == "fresh-schema69-install"
                            else truth
                        ),
                        "restoreGeneration": (
                            index
                            if phase_id.endswith("isolated-restore")
                            else 0
                        ),
                        "databaseSha256": hashlib.sha256(phase_id.encode()).hexdigest(),
                    }
                )
            report = {
                "schema": 1,
                "type": "SchemaCloneRestoreMeasurementsV1",
                "fullGitSha": sha,
                "runToken": "1" * 32,
                "attemptCount": 1,
                "retryCount": 0,
                "skipCount": 0,
                "sourceSchema": 65,
                "targetSchema": 69,
                "sourceSegmentCount": segment_count,
                "cloneSegmentCount": segment_count,
                "authoritativeTruthDigest": truth,
                "phases": phases,
                "snapshots": [
                    {
                        "kind": kind,
                        "volumeIdentitySha256": volume,
                        "manifestSha256": hashlib.sha256((kind + "m").encode()).hexdigest(),
                        "databaseSha256": hashlib.sha256((kind + "d").encode()).hexdigest(),
                        "schema": 69,
                        "segmentCount": segment_count,
                        "truthDigest": truth,
                    }
                    for kind, volume in (("local", "a" * 64), ("offsite", "b" * 64))
                ],
                "passed": True,
                "failures": [],
            }
            report_path = root / "schema-clone-and-restore.json"
            self.supervisor.atomic_write_json(report_path, report)
            paths["schema-clone-and-restore.json"] = report_path
            proof_projection = {
                "campaignSegments": segment_count,
                "scaleSegments": 30_373,
            }
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaisesRegex(
                self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
            ):
                self.verify._validate_schema_restore_semantics(paths, expected_sha=sha)
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_schema_restore_semantics(
                    paths, expected_sha=sha, expected_run_token="f" * 32
                )
            mutations = {}
            omitted = json.loads(json.dumps(report))
            omitted["phases"].pop(3)
            mutations["omitted-phase"] = omitted
            accepted_future = json.loads(json.dumps(report))
            future = next(
                item for item in accepted_future["phases"] if item["id"] == "future-schema-refusal"
            )
            future["schemaAfter"] = 69
            mutations["future-schema-accepted"] = accepted_future
            same_volume = json.loads(json.dumps(report))
            same_volume["snapshots"][1]["volumeIdentitySha256"] = "a" * 64
            mutations["same-volume"] = same_volume
            for label, forged in mutations.items():
                self.supervisor.atomic_write_json(report_path, forged)
                with self.subTest(label=label), mock.patch.object(
                    self.verify,
                    "_validate_owner_proof_binding",
                    return_value=proof_projection,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_schema_restore_semantics(paths, expected_sha=sha)

    def test_concurrency_performance_semantics_recompute_raw_p95_duration_and_heap(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self._write_owner_proof_fixture(root / "owner-proof", sha)
            report = {
                "schema": 1,
                "type": "ConcurrencyPerformanceMeasurementsV1",
                "fullGitSha": sha,
                "runToken": "2" * 32,
                "attemptCount": 1,
                "retryCount": 0,
                "skipCount": 0,
                "fixedSeed": 3_232_997_711,
                "hammer": {
                    "segmentCount": 50_000,
                    "durationSeconds": 1_800,
                    "reviewWorkers": 4,
                    "importWorkers": 2,
                    "backupWorkers": 1,
                    "expectedWrites": 10_000,
                    "committedWrites": 10_000,
                    "lockFailures": 0,
                    "lostWrites": 0,
                    "staleClobbers": 0,
                    "invalidRestoreAdmissions": 0,
                    "integrityCheck": "ok",
                    "foreignKeyViolations": 0,
                    "durableDecisionMilliseconds": [100] * 10_000,
                    "queueMilliseconds": [200] * 10_000,
                },
                "frontend": {
                    "segmentCount": 100_000,
                    "decisionCount": 1_000,
                    "initialJavaScriptGzipBytes": 120 * 1024,
                    "initialCssGzipBytes": 14 * 1024,
                    "coldShellInteractiveMilliseconds": 900,
                    "reviewUsableMilliseconds": 1_400,
                    "searchFilterMilliseconds": [100] * 1_000,
                    "actionFeedbackMilliseconds": [80] * 1_000,
                    "sameSourceAudioMilliseconds": [200] * 1_000,
                    "newSourceAudioMilliseconds": [600] * 1_000,
                    "interactionTaskMilliseconds": [40] * 1_000,
                    "scrollFramesPerSecond": [60] * 1_000,
                    "retainedHeapStartBytes": 100_000_000,
                    "retainedHeapEndBytes": 110_000_000,
                    "residentListPages": 3,
                    "residentPrefetchedClips": 3,
                },
                "passed": True,
                "failures": [],
            }
            report_path = root / "concurrency-performance-and-memory.json"
            self.supervisor.atomic_write_json(report_path, report)
            paths["concurrency-performance-and-memory.json"] = report_path
            proof_projection = {"campaignSegments": 43_774, "scaleSegments": 30_373}
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaisesRegex(
                self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
            ):
                self.verify._validate_concurrency_performance_semantics(paths, expected_sha=sha)
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_concurrency_performance_semantics(
                    paths, expected_sha=sha, expected_run_token="f" * 32
                )
            mutations = {}
            short = json.loads(json.dumps(report))
            short["hammer"]["durationSeconds"] = 1_799
            mutations["short-hammer"] = short
            lost = json.loads(json.dumps(report))
            lost["hammer"]["lostWrites"] = 1
            mutations["lost-write"] = lost
            omitted_sample = json.loads(json.dumps(report))
            omitted_sample["hammer"]["durableDecisionMilliseconds"].pop()
            mutations["omitted-write-sample"] = omitted_sample
            p95 = json.loads(json.dumps(report))
            p95["frontend"]["searchFilterMilliseconds"][-51:] = [151] * 51
            mutations["p95"] = p95
            heap = json.loads(json.dumps(report))
            heap["frontend"]["retainedHeapEndBytes"] = 100_000_000 + 20 * 1024 * 1024
            mutations["heap"] = heap
            javascript = json.loads(json.dumps(report))
            javascript["frontend"]["initialJavaScriptGzipBytes"] = 125 * 1024 + 1
            mutations["javascript-budget"] = javascript
            css = json.loads(json.dumps(report))
            css["frontend"]["initialCssGzipBytes"] = 15 * 1024 + 1
            mutations["css-budget"] = css
            seed = json.loads(json.dumps(report))
            seed["fixedSeed"] += 1
            mutations["seed"] = seed
            for label, forged in mutations.items():
                self.supervisor.atomic_write_json(report_path, forged)
                with self.subTest(label=label), mock.patch.object(
                    self.verify,
                    "_validate_owner_proof_binding",
                    return_value=proof_projection,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_concurrency_performance_semantics(paths, expected_sha=sha)

    def test_owner_workflow_semantics_reject_missing_step_retry_wrong_champion_and_truth_loss(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self._write_owner_proof_fixture(root / "owner-proof", sha)
            export_hash = "e" * 64
            report = {
                "schema": 1,
                "type": "OwnerWorkflowRecoveryMeasurementsV1",
                "fullGitSha": sha,
                "runToken": "3" * 32,
                "attemptCount": 1,
                "retryCount": 0,
                "skipCount": 0,
                "executable": {"sha256": "a" * 64, "bytes": 1_000, "buildGitSha": sha},
                "databaseSchema": 69,
                "champion": {
                    "modelId": "omniasr-7b",
                    "deploymentSha256": "b" * 64,
                    "servedDeploymentSha256": "b" * 64,
                    "exactIdentityMatched": True,
                    "hardStopBeforeTruthOnMismatch": True,
                },
                "mediaRoles": ["real-media-mp4", "real-media-mov", "real-media-flac"],
                "workflowSteps": [
                    {
                        "id": step_id,
                        "status": "PASS",
                        "attemptCount": 1,
                        "retryCount": 0,
                        "skipCount": 0,
                        "operationId": f"00000000-0000-4000-8000-{index:012x}",
                        "artifactSha256": hashlib.sha256(step_id.encode()).hexdigest(),
                    }
                    for index, step_id in enumerate(self.verify.OWNER_WORKFLOW_STEPS, start=1)
                ],
                "recoveryDrills": [
                    {
                        "id": drill_id,
                        "status": "PASS",
                        "attemptCount": 1,
                        "retryCount": 0,
                        "skipCount": 0,
                        "hardStoppedBeforeTruth": drill_id
                        in {"wsl-unavailable-hard-stop", "wrong-model-hard-stop"},
                        "draftRetained": True,
                        "databaseIntegrity": "ok",
                        "lostDecisions": 0,
                        "duplicateDecisions": 0,
                    }
                    for drill_id in self.verify.OWNER_RECOVERY_DRILLS
                ],
                "truthInvariants": {
                    "lostDecisions": 0,
                    "duplicateDecisions": 0,
                    "misattributedDecisions": 0,
                    "unpaidExternalDecisions": 0,
                    "silentCorruptions": 0,
                    "placeholderTruthRows": 0,
                },
                "exportBeforeRestartSha256": export_hash,
                "exportAfterRestartSha256": export_hash,
                "passed": True,
                "failures": [],
            }
            report_path = root / "owner-workflow-and-recovery.json"
            self.supervisor.atomic_write_json(report_path, report)
            paths["owner-workflow-and-recovery.json"] = report_path
            proof_projection = {"campaignSegments": 43_774, "scaleSegments": 30_373}
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaisesRegex(
                self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
            ):
                self.verify._validate_owner_workflow_semantics(paths, expected_sha=sha)
            with mock.patch.object(
                self.verify,
                "_validate_owner_proof_binding",
                return_value=proof_projection,
            ), self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_owner_workflow_semantics(
                    paths, expected_sha=sha, expected_run_token="f" * 32
                )
            mutations = {}
            omitted = json.loads(json.dumps(report))
            omitted["workflowSteps"].pop()
            mutations["omitted-step"] = omitted
            retried = json.loads(json.dumps(report))
            retried["recoveryDrills"][2]["retryCount"] = 1
            mutations["retry"] = retried
            wrong = json.loads(json.dumps(report))
            wrong["champion"]["servedDeploymentSha256"] = "c" * 64
            mutations["wrong-champion"] = wrong
            lost = json.loads(json.dumps(report))
            lost["truthInvariants"]["lostDecisions"] = 1
            mutations["lost-decision"] = lost
            export = json.loads(json.dumps(report))
            export["exportAfterRestartSha256"] = "f" * 64
            mutations["export-drift"] = export
            for label, forged in mutations.items():
                self.supervisor.atomic_write_json(report_path, forged)
                with self.subTest(label=label), mock.patch.object(
                    self.verify,
                    "_validate_owner_proof_binding",
                    return_value=proof_projection,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_owner_workflow_semantics(paths, expected_sha=sha)

    def test_owner_field_sessions_semantics_require_thirty_hash_chained_incident_free_days(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ledger = root / "owner-field-sessions.jsonl"
            summary_path = root / "owner-field-session-summary.json"
            previous = "0" * 64
            records = []
            base = datetime.now(timezone.utc).replace(microsecond=0) - timedelta(days=31)
            for ordinal in range(1, 31):
                started = base + timedelta(days=ordinal, hours=1)
                ended = started + timedelta(minutes=30)
                record = {
                    "schema": 1,
                    "type": "AutomaticOwnerFieldSessionV1",
                    "sessionId": f"00000000-0000-4000-8000-{ordinal:012x}",
                    "ordinal": ordinal,
                    "fullGitSha": sha,
                    "executableSha256": "a" * 64,
                    "databaseSchema": 69,
                    "startedAt": self.verify._format_utc(started),
                    "endedAt": self.verify._format_utc(ended),
                    "durableDecisionCount": 3,
                    "playbackCount": 3,
                    "retryCount": 0,
                    "skipCount": 0,
                    "dataLossCount": 0,
                    "duplicateDecisionCount": 0,
                    "misattributedDecisionCount": 0,
                    "silentCorruptionCount": 0,
                    "incidents": [],
                    "previousHash": previous,
                    "recordHash": "",
                }
                record["recordHash"] = self.verify._field_session_record_hash(record)
                previous = record["recordHash"]
                records.append(record)
            ledger.write_text(
                "".join(json.dumps(record, sort_keys=True) + "\n" for record in records),
                encoding="utf-8",
                newline="\n",
            )
            summary = {
                "schema": 1,
                "type": "OwnerFieldSessionSummaryV1",
                "fullGitSha": sha,
                "sessionCount": 30,
                "distinctUtcDates": 30,
                "firstStartedAt": records[0]["startedAt"],
                "lastEndedAt": records[-1]["endedAt"],
                "totalDurableDecisions": 90,
                "executableSha256": "a" * 64,
                "databaseSchema": 69,
                "finalRecordHash": previous,
                "passed": True,
                "failures": [],
            }
            self.supervisor.atomic_write_json(summary_path, summary)
            paths = {
                "owner-field-sessions.jsonl": ledger,
                "owner-field-session-summary.json": summary_path,
            }
            with self.assertRaisesRegex(
                self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
            ):
                self.verify._validate_owner_field_session_semantics(paths, expected_sha=sha)
            field_manifest = {
                "runToken": "5" * 32,
                "startedAt": records[0]["startedAt"],
                "endedAt": records[-1]["endedAt"],
            }
            with self.assertRaisesRegex(
                self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
            ):
                self.verify._validate_owner_campaign_semantics(
                    "owner-field-sessions",
                    paths,
                    manifest=field_manifest,
                    expected_sha=sha,
                    expected_checkout_digest="a" * 64,
                    require_fresh=True,
                )
            with self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_owner_campaign_semantics(
                    "owner-field-sessions",
                    paths,
                    manifest={**field_manifest, "startedAt": records[1]["startedAt"]},
                    expected_sha=sha,
                    expected_checkout_digest="a" * 64,
                    require_fresh=True,
                )
            attacks = {}
            incident = json.loads(json.dumps(records))
            incident[10]["incidents"] = [{"severity": "P1"}]
            attacks["incident"] = incident
            duplicate = json.loads(json.dumps(records))
            duplicate[1]["sessionId"] = duplicate[0]["sessionId"]
            attacks["duplicate"] = duplicate
            executable = json.loads(json.dumps(records))
            executable[-1]["executableSha256"] = "b" * 64
            attacks["executable"] = executable
            retry = json.loads(json.dumps(records))
            retry[5]["retryCount"] = 1
            attacks["retry"] = retry
            for label, forged in attacks.items():
                ledger.write_text(
                    "".join(json.dumps(record, sort_keys=True) + "\n" for record in forged),
                    encoding="utf-8",
                    newline="\n",
                )
                with self.subTest(label=label), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_owner_field_session_semantics(paths, expected_sha=sha)

    def test_owner_deployment_semantics_require_candidate_then_active_and_distinct_cold_boot(self) -> None:
        sha = self.verify._full_git_sha()
        with tempfile.TemporaryDirectory() as temporary:
            report_path = Path(temporary) / "owner-deployment-and-reboot.json"
            release_manifest = "d" * 64
            phases = [
                {
                    "id": phase_id,
                    "proofRunToken": f"{index:032x}",
                    "manifestSha256": hashlib.sha256((phase_id + "m").encode()).hexdigest(),
                    "productAttestationSha256": hashlib.sha256(
                        (phase_id + "a").encode()
                    ).hexdigest(),
                    "releaseAuthority": authority,
                    "bootIdentitySha256": boot,
                    "deployedReleaseManifestSha256": release_manifest,
                }
                for index, (phase_id, authority, boot) in enumerate(
                    (
                        ("pre-deployment", "staged-owner-candidate", "a" * 64),
                        ("post-deployment", "active-immutable-release", "a" * 64),
                        ("post-cold-reboot", "active-immutable-release", "b" * 64),
                    ),
                    start=1,
                )
            ]
            report = {
                "schema": 1,
                "type": "OwnerDeploymentRebootMeasurementsV1",
                "fullGitSha": sha,
                "runToken": "4" * 32,
                "attemptCount": 1,
                "retryCount": 0,
                "skipCount": 0,
                "executableSha256": "c" * 64,
                "executableBytes": 1_000,
                "databaseSchema": 69,
                "phases": phases,
                "passed": True,
                "failures": [],
            }
            self.supervisor.atomic_write_json(report_path, report)

            def projected(phase, **_kwargs):
                return dict(phase)

            with mock.patch.object(
                self.verify, "_validate_deployment_phase_control", side_effect=projected
            ):
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, self.verify.UNSUPPORTED_UNBACKED_EVIDENCE
                ):
                    self.verify._validate_owner_deployment_semantics(
                        {"owner-deployment-and-reboot.json": report_path},
                        manifest={"gateRegistryHash": self.verify.gate_registry_hash()},
                        expected_sha=sha,
                        expected_checkout_digest=self.verify._checkout_state_digest(),
                    )
            mutations = {}
            wrong_authority = json.loads(json.dumps(report))
            wrong_authority["phases"][0]["releaseAuthority"] = "active-immutable-release"
            mutations["predeploy-not-candidate"] = wrong_authority
            same_boot = json.loads(json.dumps(report))
            same_boot["phases"][2]["bootIdentitySha256"] = "a" * 64
            mutations["no-cold-reboot"] = same_boot
            different_release = json.loads(json.dumps(report))
            different_release["phases"][2]["deployedReleaseManifestSha256"] = "e" * 64
            mutations["release-substitution"] = different_release
            retried = json.loads(json.dumps(report))
            retried["retryCount"] = 1
            mutations["retry"] = retried
            for label, forged in mutations.items():
                self.supervisor.atomic_write_json(report_path, forged)
                with self.subTest(label=label), mock.patch.object(
                    self.verify,
                    "_validate_deployment_phase_control",
                    side_effect=projected,
                ), self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_owner_deployment_semantics(
                        {"owner-deployment-and-reboot.json": report_path},
                        manifest={"gateRegistryHash": self.verify.gate_registry_hash()},
                        expected_sha=sha,
                        expected_checkout_digest=self.verify._checkout_state_digest(),
                    )

    def test_deployment_phase_control_binds_run_mode_phase_and_staged_manifest(self) -> None:
        sha = self.verify._full_git_sha()
        registry_hash = self.verify.gate_registry_hash()
        checkout_digest = self.verify._checkout_state_digest()
        token = "1" * 32
        release_manifest_sha = "d" * 64
        candidate = {
            "releaseId": "candidate-release",
            "manifestSha256": release_manifest_sha,
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = "phases/pre-deployment/"
            paths: dict[str, Path] = {}
            for name in (
                "manifest.json",
                "product-attestation.json",
                "events.jsonl",
                "gate-registry.json",
                "environment.json",
                "evidence-contract.json",
                self.verify.RUN_AUTHORITY_NAME,
            ):
                path = root / name
                paths[prefix + name] = path
            environment: dict[str, object] = {}
            run_authority = {
                "releasePhase": "pre-deployment",
                "stagedCandidate": candidate,
            }
            application = {
                "role": "application-executable",
                "sha256": "c" * 64,
                "bytes": 1_000,
                "buildGitSha": sha,
                "matchesFullGitSha": True,
                "authority": "staged-owner-candidate",
                "releasePhase": "pre-deployment",
                "stagedReleaseId": "candidate-release",
                "stagedReleaseManifestSha256": release_manifest_sha,
            }
            results = [
                {
                    "gateId": gate.id,
                    "attemptCount": 1,
                    "retryCount": 0,
                    "retryReasons": [],
                    "status": self.verify.PASS,
                }
                for gate in self.verify.GATES
                if self.verify.PROFILE_OWNER in gate.profiles
            ]
            manifest = {
                "schema": 1,
                "complete": True,
                "runToken": token,
                "startedAt": "2026-08-28T10:00:00Z",
                "endedAt": "2026-08-28T10:01:00Z",
                "fullGitSha": sha,
                "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                "checkoutStateDigest": checkout_digest,
                "profile": self.verify.PROFILE_OWNER,
                "quick": False,
                "gateRegistryHash": registry_hash,
                "environment": environment,
                "staleTakeover": {"occurred": False, "abandonedRunToken": None},
                "runAuthority": run_authority,
                "evidenceContractHash": self.verify.evidence_contract_hash(),
                "schemaAuthority": {"latestVersion": 69},
                "releaseArtifacts": [application],
                "results": results,
            }
            self.supervisor.atomic_write_json(paths[prefix + "manifest.json"], manifest)
            self.supervisor.atomic_write_json(paths[prefix + "product-attestation.json"], {})
            self.supervisor.atomic_write_json(
                paths[prefix + "gate-registry.json"], self.verify.gate_registry_document()
            )
            self.supervisor.atomic_write_json(paths[prefix + "environment.json"], environment)
            self.supervisor.atomic_write_json(
                paths[prefix + "evidence-contract.json"], self.verify.evidence_contract_document()
            )
            self.supervisor.atomic_write_json(paths[prefix + self.verify.RUN_AUTHORITY_NAME], run_authority)
            paths[prefix + "events.jsonl"].write_text(
                "".join(
                    json.dumps(event, sort_keys=True) + "\n"
                    for event in (
                        {
                            "schema": 1,
                            "sequence": 1,
                            "runToken": token,
                            "event": "run_start",
                        },
                        {
                            "schema": 1,
                            "sequence": 2,
                            "runToken": token,
                            "event": "run_end",
                        },
                    )
                ),
                encoding="utf-8",
                newline="\n",
            )
            phase = {
                "id": "pre-deployment",
                "proofRunToken": token,
                "manifestSha256": self.supervisor.sha256_file(paths[prefix + "manifest.json"]),
                "productAttestationSha256": self.supervisor.sha256_file(
                    paths[prefix + "product-attestation.json"]
                ),
                "releaseAuthority": "staged-owner-candidate",
                "bootIdentitySha256": "a" * 64,
                "deployedReleaseManifestSha256": release_manifest_sha,
            }

            def validate(*, authority_mode="staged-owner-candidate", candidate_value=candidate):
                with mock.patch.object(
                    self.verify,
                    "_validate_run_authority",
                    return_value=(authority_mode, "e" * 64),
                ), mock.patch.object(
                    self.verify,
                    "_validate_product_attestation",
                ), mock.patch.object(
                    self.verify,
                    "_validate_staged_candidate_authority",
                    return_value=candidate_value,
                ):
                    return self.verify._validate_deployment_phase_control(
                        phase,
                        paths=paths,
                        expected_sha=sha,
                        expected_registry_hash=registry_hash,
                        expected_checkout_digest=checkout_digest,
                        expected_environment=environment,
                        expected_executable_sha256="c" * 64,
                        expected_executable_bytes=1_000,
                    )

            validate()
            with self.assertRaises(self.verify.EvidenceError):
                validate(authority_mode="windows-known-folders-live")
            run_authority["releasePhase"] = "routine"
            self.supervisor.atomic_write_json(paths[prefix + self.verify.RUN_AUTHORITY_NAME], run_authority)
            manifest["runAuthority"] = run_authority
            self.supervisor.atomic_write_json(paths[prefix + "manifest.json"], manifest)
            phase["manifestSha256"] = self.supervisor.sha256_file(paths[prefix + "manifest.json"])
            with self.assertRaises(self.verify.EvidenceError):
                validate()
            run_authority["releasePhase"] = "pre-deployment"
            self.supervisor.atomic_write_json(paths[prefix + self.verify.RUN_AUTHORITY_NAME], run_authority)
            manifest["runAuthority"] = run_authority
            self.supervisor.atomic_write_json(paths[prefix + "manifest.json"], manifest)
            phase["manifestSha256"] = self.supervisor.sha256_file(paths[prefix + "manifest.json"])
            with self.assertRaises(self.verify.EvidenceError):
                validate(candidate_value={**candidate, "manifestSha256": "f" * 64})

    def test_missing_genuine_owner_campaigns_emit_required_durable_red_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "absent"
            source.mkdir()
            original = (
                self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT,
                self.verify.LOG_DIR,
                self.verify._ACTIVE_WORKER_PROFILE,
            )
            try:
                self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT = source
                self.verify._ACTIVE_WORKER_PROFILE = self.verify.PROFILE_OWNER
                for index, class_id in enumerate(
                    self.verify.OWNER_EVIDENCE_CLASS_GATE_IDS, start=1
                ):
                    gate_root = root / f"red-{index}"
                    gate_root.mkdir()
                    self.verify.LOG_DIR = gate_root
                    self.assertFalse(self.verify._fn_owner_evidence_class(class_id), class_id)
                    artifact = gate_root / self.verify.OWNER_EVIDENCE_CLASS_ARTIFACTS[class_id]
                    self.assertTrue(artifact.is_file(), class_id)
                    value = self.verify._load_json_without_duplicate_keys(artifact)
                    self.assertEqual(value["classId"], class_id)
                    self.assertFalse(value["passed"])
                    self.assertEqual(value["machineArtifacts"], [])
                    self.assertTrue(value["failures"])
            finally:
                (
                    self.verify.OWNER_EVIDENCE_AUTHORITY_ROOT,
                    self.verify.LOG_DIR,
                    self.verify._ACTIVE_WORKER_PROFILE,
                ) = original

    def test_fault_campaign_evidence_rejects_self_authored_retry_stale_and_incomplete_reports(self) -> None:
        sha = self.verify._full_git_sha()
        registry_hash = self.verify.gate_registry_hash()
        checkout_digest = self.verify._checkout_state_digest()
        environment = self.verify._environment_document()
        now = datetime.now(timezone.utc).replace(microsecond=0)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            campaign_root = root / "campaigns"
            gate_root = root / "gate"
            gate_root.mkdir()
            for index, minutes in enumerate((12, 8, 4), start=1):
                self._write_fault_campaign(
                    campaign_root,
                    token=f"{index:032x}",
                    started=now - timedelta(minutes=minutes),
                    sha=sha,
                    registry_hash=registry_hash,
                    checkout_digest=checkout_digest,
                    environment=environment,
                )
            original = self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT, self.verify.LOG_DIR
            try:
                self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT = campaign_root
                self.verify.LOG_DIR = gate_root
                artifact = self.verify._build_verifier_fault_campaign_evidence()
            finally:
                self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT, self.verify.LOG_DIR = original
            path = gate_root / self.verify._FAULT_CAMPAIGNS_ARTIFACT
            self.supervisor.atomic_write_json(path, artifact)

            def validate() -> None:
                self.verify._validate_class_evidence_artifact(
                    "verifier-fault-campaigns",
                    path,
                    expected_sha=sha,
                    expected_profile=self.verify.PROFILE_OWNER,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                )

            validate()
            mutations = {}
            retried = json.loads(json.dumps(artifact))
            retried["campaigns"][1]["retryCount"] = 1
            mutations["retry"] = retried
            incomplete = json.loads(json.dumps(artifact))
            incomplete["campaigns"] = incomplete["campaigns"][:2]
            mutations["incomplete"] = incomplete
            scenario_omission = json.loads(json.dumps(artifact))
            scenario_omission["campaigns"][0]["scenarioResults"].pop()
            mutations["scenario"] = scenario_omission
            stale = json.loads(json.dumps(artifact))
            stale["expiresAt"] = self.verify._format_utc(now - timedelta(seconds=1))
            mutations["stale"] = stale
            for label, mutation in mutations.items():
                with self.subTest(label=label):
                    self.supervisor.atomic_write_json(path, mutation)
                    with self.assertRaises(self.verify.EvidenceError):
                        validate()
            self_authored_root = root / "self-authored"
            self_authored_root.mkdir()
            self_authored_path = self_authored_root / self.verify._FAULT_CAMPAIGNS_ARTIFACT
            self.supervisor.atomic_write_json(self_authored_path, artifact)
            with self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_class_evidence_artifact(
                    "verifier-fault-campaigns",
                    self_authored_path,
                    expected_sha=sha,
                    expected_profile=self.verify.PROFILE_OWNER,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                )
            self.supervisor.atomic_write_json(path, artifact)
            copied_log = next(
                gate_root.glob(
                    f"{self.verify.MACHINE_EVIDENCE_DIRECTORY}/verifier-fault-campaigns/*/"
                    f"{self.verify.VERIFIER_FAULT_CAMPAIGN_LOG}"
                )
            )
            copied_log.write_text("self-authored pass\n", encoding="utf-8")
            with self.assertRaises(self.verify.EvidenceError):
                validate()

            future_manifest = self._write_fault_campaign(
                root / "future-campaign",
                token="e" * 32,
                started=now + timedelta(hours=1),
                sha=sha,
                registry_hash=registry_hash,
                checkout_digest=checkout_digest,
                environment=environment,
            )
            with self.assertRaisesRegex(self.verify.EvidenceError, "chronology"):
                self.verify._validate_fault_campaign_manifest(
                    future_manifest,
                    expected_sha=sha,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                    require_fresh=True,
                    require_pass=True,
                )
            boolean_manifest = self._write_fault_campaign(
                root / "boolean-campaign",
                token="d" * 32,
                started=now - timedelta(minutes=2),
                sha=sha,
                registry_hash=registry_hash,
                checkout_digest=checkout_digest,
                environment=environment,
            )
            boolean_value = json.loads(boolean_manifest.read_text(encoding="utf-8"))
            boolean_value["schema"] = True
            self.supervisor.atomic_write_json(boolean_manifest, boolean_value)
            with self.assertRaisesRegex(self.verify.EvidenceError, "schema/type/run"):
                self.verify._validate_fault_campaign_manifest(
                    boolean_manifest,
                    expected_sha=sha,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                    require_fresh=True,
                    require_pass=True,
                )

            incomplete_token = "f" * 32
            incomplete = campaign_root / incomplete_token
            incomplete.mkdir()
            incomplete_started = self.verify._format_utc(now - timedelta(minutes=1))
            incomplete_event = {
                "schema": 1,
                "sequence": 1,
                "runToken": incomplete_token,
                "event": "campaign_start",
                "at": incomplete_started,
                "fullGitSha": sha,
                "sourceTreeDigest": self.verify._source_tree_digest_for_sha(sha),
                "checkoutStateDigest": checkout_digest,
                "gateRegistryHash": registry_hash,
                "environmentDigest": self.verify._document_digest(environment),
                "attemptCount": 1,
                "retryPolicy": "none",
            }
            (incomplete / "events.jsonl").write_text(
                json.dumps(incomplete_event, sort_keys=True) + "\n",
                encoding="utf-8",
                newline="\n",
            )
            retry_gate_root = root / "retry-gate"
            retry_gate_root.mkdir()
            original = self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT, self.verify.LOG_DIR
            try:
                self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT = campaign_root
                self.verify.LOG_DIR = retry_gate_root
                self.assertEqual(
                    len(
                        self.verify._matching_fault_campaign_attempts(
                            expected_sha=sha,
                            expected_registry_hash=registry_hash,
                            expected_checkout_digest=checkout_digest,
                            expected_environment=environment,
                        )
                    ),
                    4,
                )
                # The builder must reject the incomplete fourth attempt. Depending on whether the
                # earlier tamper probe invalidated the three copied exact-authority logs first, the
                # fail-closed diagnostic may report either missing exact attempts or incompleteness.
                with self.assertRaises(self.verify.EvidenceError):
                    self.verify._build_verifier_fault_campaign_evidence()
            finally:
                self.verify.VERIFIER_FAULT_CAMPAIGN_ROOT, self.verify.LOG_DIR = original

    def test_timeout_calibration_evidence_recomputes_every_gate_and_rejects_retries(self) -> None:
        profile = self.verify.PROFILE_OWNER
        sha = self.verify._full_git_sha()
        registry_hash = self.verify.gate_registry_hash()
        checkout_digest = self.verify._checkout_state_digest()
        environment = self.verify._environment_document()
        selected = [gate for gate in self.verify.GATES if profile in gate.profiles]
        calibrated = [
            gate for gate in selected if gate.id != "timeout-calibration-evidence"
        ]
        observations = {
            gate.id: 0.0 if gate.timeout_seconds == 120 else gate.timeout_seconds / 4.0
            for gate in calibrated
        }
        for gate in calibrated:
            self.assertGreaterEqual(
                gate.timeout_seconds,
                self.verify._required_calibrated_timeout(observations[gate.id]),
                gate.id,
            )
        now = datetime.now(timezone.utc).replace(microsecond=0)
        baselines = []
        for index, minutes in enumerate((12, 8, 4), start=1):
            started = now - timedelta(minutes=minutes)
            ended = started + timedelta(minutes=1)
            token = f"{index + 20:032x}"
            gate_results = []
            for gate in selected:
                gate_results.append(
                    {
                        "gateId": gate.id,
                        "status": (
                            self.verify.FAIL
                            if gate.id == "timeout-calibration-evidence"
                            else self.verify.PASS
                        ),
                        "seconds": observations.get(gate.id, 0.0),
                    }
                )
            baselines.append(
                {
                    "runToken": token,
                    "manifestSha256": hashlib.sha256((token + "m").encode()).hexdigest(),
                    "productAttestationSha256": hashlib.sha256(
                        (token + "a").encode()
                    ).hexdigest(),
                    "startedAt": self.verify._format_utc(started),
                    "endedAt": self.verify._format_utc(ended),
                    "expiresAt": self.verify._format_utc(
                        ended
                        + timedelta(
                            seconds=self.verify.TIMEOUT_CALIBRATION_FRESH_SECONDS
                        )
                    ),
                    "attemptCount": 1,
                    "retryCount": 0,
                    "staleTakeover": False,
                    "gateResults": gate_results,
                }
            )
        calibrations = [
            {
                "gateId": gate.id,
                "observedSeconds": [observations[gate.id]] * 3,
                "observedMaximumSeconds": observations[gate.id],
                "requiredTimeoutSeconds": gate.timeout_seconds,
                "configuredTimeoutSeconds": gate.timeout_seconds,
            }
            for gate in calibrated
        ]
        artifact = {
            "schema": 1,
            "classId": "timeout-calibration-baselines",
            "fullGitSha": sha,
            "gateRegistryHash": registry_hash,
            "checkoutStateDigest": checkout_digest,
            "environment": environment,
            "environmentDigest": self.verify._document_digest(environment),
            "profile": profile,
            "measuredAt": baselines[-1]["endedAt"],
            "expiresAt": baselines[0]["expiresAt"],
            "immutableAuthority": "exact-git-commit",
            "formula": "ceil(max(3 * observedMaximumSeconds, observedMaximumSeconds + 120))",
            "excludedSelfGateId": "timeout-calibration-evidence",
            "selectedBudgetSeconds": sum(gate.timeout_seconds for gate in selected),
            "baselines": baselines,
            "calibrations": calibrations,
            "passed": True,
            "failures": [],
        }
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / self.verify._TIMEOUT_CALIBRATION_ARTIFACT
            self.supervisor.atomic_write_json(path, artifact)
            with self.assertRaises(self.verify.EvidenceError):
                self.verify._validate_class_evidence_artifact(
                    "timeout-calibration-baselines",
                    path,
                    expected_sha=sha,
                    expected_profile=profile,
                    expected_registry_hash=registry_hash,
                    expected_checkout_digest=checkout_digest,
                    expected_environment=environment,
                )
            mutations = {}
            retried = json.loads(json.dumps(artifact))
            first_workload = next(
                item
                for item in retried["baselines"][0]["gateResults"]
                if item["gateId"] != "timeout-calibration-evidence"
            )
            first_workload["status"] = self.verify.PASS_AFTER_RETRY
            mutations["retry-status"] = retried
            retry_count = json.loads(json.dumps(artifact))
            retry_count["baselines"][0]["retryCount"] = 1
            mutations["retry-count"] = retry_count
            incomplete = json.loads(json.dumps(artifact))
            incomplete["baselines"].pop()
            mutations["incomplete"] = incomplete
            substituted_registry = json.loads(json.dumps(artifact))
            substituted_registry["gateRegistryHash"] = "0" * 64
            mutations["registry"] = substituted_registry
            stale = json.loads(json.dumps(artifact))
            stale["expiresAt"] = self.verify._format_utc(now - timedelta(seconds=1))
            mutations["stale"] = stale
            under_budget = json.loads(json.dumps(artifact))
            first_calibration = under_budget["calibrations"][0]
            configured = first_calibration["configuredTimeoutSeconds"]
            under_budget_observation = max(1.0, (configured + 1.0) / 3.0)
            required = self.verify._required_calibrated_timeout(under_budget_observation)
            self.assertGreater(required, configured)
            for baseline in under_budget["baselines"]:
                next(
                    item
                    for item in baseline["gateResults"]
                    if item["gateId"] == first_calibration["gateId"]
                )["seconds"] = under_budget_observation
            first_calibration["observedSeconds"] = [under_budget_observation] * 3
            first_calibration["observedMaximumSeconds"] = under_budget_observation
            first_calibration["requiredTimeoutSeconds"] = required
            mutations["under-budget"] = under_budget
            for label, mutation in mutations.items():
                with self.subTest(label=label):
                    self.supervisor.atomic_write_json(path, mutation)
                    with self.assertRaises(self.verify.EvidenceError):
                        self.verify._validate_class_evidence_artifact(
                            "timeout-calibration-baselines",
                            path,
                            expected_sha=sha,
                            expected_profile=profile,
                            expected_registry_hash=registry_hash,
                            expected_checkout_digest=checkout_digest,
                            expected_environment=environment,
                        )

    @_requires_windows_live_authority
    def test_timeout_calibration_consumes_completed_manifests_and_latest_incomplete_blocks(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixed_checkout_digest = self.verify._checkout_state_digest()
            manifest_gate = replace(
                self.verify._gate_by_id("manifest-alignment"),
                timeout_seconds=121,
            )
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
                self.verify.LOG_DIR,
                self.verify._assert_source_state,
                self.verify._checkout_state_digest,
            )
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = [manifest_gate]
                self.verify.LOG_DIR = root / "gate"
                self.verify.LOG_DIR.mkdir()
                self.verify._assert_source_state = lambda *_args: None
                self.verify._checkout_state_digest = lambda: fixed_checkout_digest
                with mock.patch.object(
                    self.verify,
                    "_consume_rust_coverage_prerequisite",
                    side_effect=self._synthetic_coverage_binding,
                ), mock.patch.object(
                    self.verify,
                    "_release_artifact_bindings",
                    side_effect=self._stable_active_release_binding,
                ):
                    for _index in range(3):
                        self.assertEqual(
                            self.verify.aggregate_main(
                                quick=False,
                                status_md=None,
                                profile=self.verify.PROFILE_OWNER,
                            ),
                            2,
                        )
                first_source_attestation = next(
                    run_dir / self.verify.PRODUCT_ATTESTATION_NAME
                    for run_dir in self.verify.PROOF_ROOT.iterdir()
                )
                original_source_attestation = first_source_attestation.read_bytes()
                self.supervisor.atomic_write_json(first_source_attestation, {})
                evidence_gate_root = self.verify.LOG_DIR
                self.verify.LOG_DIR = root / "tamper-gate"
                self.verify.LOG_DIR.mkdir()
                with self.assertRaises(self.verify.EvidenceError):
                    self.verify._build_timeout_calibration_evidence(
                        profile=self.verify.PROFILE_OWNER,
                        current_run_token=None,
                    )
                self.supervisor.atomic_write_bytes(
                    first_source_attestation, original_source_attestation
                )
                self.verify.LOG_DIR = evidence_gate_root
                report = self.verify._build_timeout_calibration_evidence(
                    profile=self.verify.PROFILE_OWNER,
                    current_run_token=None,
                )
                self.assertTrue(report["passed"])
                self.assertEqual(len(report["baselines"]), 3)
                self.assertEqual(len(report["calibrations"]), 1)
                calibration = report["calibrations"][0]
                self.assertEqual(calibration["gateId"], "manifest-alignment")
                self.assertEqual(calibration["requiredTimeoutSeconds"], 121)
                self.assertEqual(calibration["configuredTimeoutSeconds"], 121)
                self.assertEqual(len(calibration["observedSeconds"]), 3)
                report_path = self.verify.LOG_DIR / self.verify._TIMEOUT_CALIBRATION_ARTIFACT
                self.supervisor.atomic_write_json(report_path, report)
                self.verify._validate_class_evidence_artifact(
                    "timeout-calibration-baselines",
                    report_path,
                    expected_sha=self.verify._full_git_sha(),
                    expected_profile=self.verify.PROFILE_OWNER,
                    expected_registry_hash=self.verify.gate_registry_hash(),
                    expected_checkout_digest=fixed_checkout_digest,
                    expected_environment=self.verify._environment_document(),
                )
                first_attestation = next(
                    self.verify.LOG_DIR.glob(
                        f"{self.verify.MACHINE_EVIDENCE_DIRECTORY}/"
                        "timeout-calibration-baselines/*/product-attestation.json"
                    )
                )
                original_attestation = first_attestation.read_bytes()
                self.supervisor.atomic_write_json(first_attestation, {})
                with self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_class_evidence_artifact(
                        "timeout-calibration-baselines",
                        report_path,
                        expected_sha=self.verify._full_git_sha(),
                        expected_profile=self.verify.PROFILE_OWNER,
                        expected_registry_hash=self.verify.gate_registry_hash(),
                        expected_checkout_digest=fixed_checkout_digest,
                        expected_environment=self.verify._environment_document(),
                    )
                self.supervisor.atomic_write_bytes(first_attestation, original_attestation)

                latest_start = max(
                    json.loads(
                        (run_dir / "events.jsonl")
                        .read_text(encoding="utf-8")
                        .splitlines()[0]
                    )["at"]
                    for run_dir in self.verify.PROOF_ROOT.iterdir()
                )
                incomplete_token = "f" * 32
                incomplete = self.verify.PROOF_ROOT / incomplete_token
                incomplete.mkdir()
                start_event = {
                    "schema": 1,
                    "sequence": 1,
                    "runToken": incomplete_token,
                    "event": "run_start",
                    "at": latest_start,
                    "fullGitSha": self.verify._full_git_sha(),
                    "sourceTreeDigest": self.verify._source_tree_digest(),
                    "checkoutStateDigest": fixed_checkout_digest,
                    "profile": self.verify.PROFILE_OWNER,
                    "quick": False,
                    "gateRegistryHash": self.verify.gate_registry_hash(),
                    "authorityMode": self.verify.AUTHORITY_MODE_LIVE,
                    "runAuthorityDigest": "0" * 64,
                }
                (incomplete / "events.jsonl").write_text(
                    json.dumps(start_event) + "\n",
                    encoding="utf-8",
                )
                self.supervisor.atomic_write_json(
                    incomplete / "environment.json",
                    self.verify._environment_document(),
                )
                self.verify.LOG_DIR = root / "retry-gate"
                self.verify.LOG_DIR.mkdir()
                with self.assertRaisesRegex(self.verify.EvidenceError, "incomplete"):
                    self.verify._build_timeout_calibration_evidence(
                        profile=self.verify.PROFILE_OWNER,
                        current_run_token=None,
                    )
                (self.verify.PROOF_ROOT / ("e" * 32)).mkdir()
                self.verify.LOG_DIR = root / "missing-start-gate"
                self.verify.LOG_DIR.mkdir()
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "no durable run_start authority"
                ):
                    self.verify._build_timeout_calibration_evidence(
                        profile=self.verify.PROFILE_OWNER,
                        current_run_token=None,
                    )
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                    self.verify.LOG_DIR,
                    self.verify._assert_source_state,
                    self.verify._checkout_state_digest,
                ) = original

    def test_known_defect_validator_blocks_supported_p2_but_keeps_disabled_scope_explicit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ledger_path = root / "known.json"
            artifact_dir = root / "artifacts"
            artifact_dir.mkdir()
            ledger = json.loads(self.verify.KNOWN_DEFECT_LEDGER.read_text(encoding="utf-8"))
            ledger["defects"][0]["severity"] = "P2"
            ledger_path.write_text(json.dumps(ledger), encoding="utf-8")
            binding = {
                "path": "authority.md",
                "gitBlobSha1": "b" * 40,
                "sha256": "c" * 64,
                "bytes": 1,
            }
            patches = (
                mock.patch.object(self.verify, "KNOWN_DEFECT_LEDGER", ledger_path),
                mock.patch.object(self.verify, "LOG_DIR", artifact_dir),
                mock.patch.object(self.verify, "_full_git_sha", return_value="a" * 40),
                mock.patch.object(self.verify, "_safe_tracked_path", return_value=ledger_path),
                mock.patch.object(
                    self.verify,
                    "_tracked_authority_binding",
                    return_value=binding,
                ),
            )
            with patches[0], patches[1], patches[2], patches[3], patches[4]:
                self.assertFalse(self.verify._fn_known_defect_ledger())
            report = json.loads(
                (artifact_dir / self.verify._KNOWN_DEFECT_ARTIFACT).read_text(encoding="utf-8")
            )
            self.assertEqual(report["blockingDefectIds"], ["ARCH-IPC-001"])

            ledger["defects"][0]["severity"] = "P3"
            ledger_path.write_text(json.dumps(ledger), encoding="utf-8")
            with patches[0], patches[1], patches[2], patches[3], patches[4]:
                self.assertTrue(self.verify._fn_known_defect_ledger())
            report = json.loads(
                (artifact_dir / self.verify._KNOWN_DEFECT_ARTIFACT).read_text(encoding="utf-8")
            )
            self.assertEqual(report["blockingDefectIds"], [])
            disabled_p2 = next(
                item for item in report["defects"] if item["id"] == "POOL-PAY-POLICY-001"
            )
            self.assertEqual(disabled_p2["supportedProfiles"], [])

            ledger["schema"] = True
            ledger_path.write_text(json.dumps(ledger), encoding="utf-8")
            with patches[0], patches[1], patches[2], patches[3], patches[4]:
                self.assertFalse(self.verify._fn_known_defect_ledger())
            report = json.loads(
                (artifact_dir / self.verify._KNOWN_DEFECT_ARTIFACT).read_text(encoding="utf-8")
            )
            self.assertIn("schema is not 1", report["failures"][0])

    def test_tracked_authority_binding_uses_canonical_git_bytes_across_windows_eol(self) -> None:
        committed = b"line one\nline two\n"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            authority = root / "authority.md"
            authority.write_bytes(committed.replace(b"\n", b"\r\n"))
            with (
                mock.patch.object(self.verify, "REPO_ROOT", root),
                mock.patch.object(self.verify, "_git_file_bytes", return_value=committed),
                mock.patch.object(self.verify, "_git_blob_id", return_value="b" * 40),
            ):
                binding = self.verify._tracked_authority_binding(authority, "a" * 40)
            self.assertEqual(binding["sha256"], hashlib.sha256(committed).hexdigest())
            self.assertEqual(binding["bytes"], len(committed))
            self.assertEqual(binding["gitBlobSha1"], "b" * 40)

    def test_windows_proof_consumer_rebinds_every_required_role_to_measured_bundle(self) -> None:
        profile = self.verify.PROFILE_WINDOWS
        sha = "a" * 40
        roles = self.verify.PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[profile]
        artifacts = [
            {
                "role": role,
                "name": f"{role}.bin",
                "path": f"{role}.bin",
                "sha256": hashlib.sha256(role.encode("utf-8")).hexdigest(),
                "bytes": len(role),
            }
            for role in roles
        ]
        authority = {
            "schema": 1,
            "type": "WindowsReleaseBundleAuthorityV1",
            "source": {
                "fullGitSha": sha,
                "repository": "owner/cortex-speech",
                "ref": "refs/tags/v1.2.3",
                "version": "1.2.3",
            },
            "signer": {
                "thumbprintSha1": "A" * 40,
                "certificateSha256": "B" * 64,
            },
            "cryptographicValidation": {
                "authenticodeAndTimestampVerified": True,
                "githubSigstoreProvenanceVerified": True,
            },
            "certificationReady": True,
            "artifacts": artifacts,
        }
        manifest = {
            "profile": profile,
            "fullGitSha": sha,
            "releaseArtifacts": [{**artifact, "proofOnlyField": True} for artifact in artifacts],
        }
        self.verify._bind_proof_to_windows_release_bundle(manifest, authority)

        forged = json.loads(json.dumps(authority))
        forged["artifacts"][1]["sha256"] = "0" * 64
        with self.assertRaisesRegex(self.verify.EvidenceError, "differs from the re-observed"):
            self.verify._bind_proof_to_windows_release_bundle(manifest, forged)

        no_crypto = json.loads(json.dumps(authority))
        no_crypto["cryptographicValidation"]["authenticodeAndTimestampVerified"] = False
        with self.assertRaisesRegex(self.verify.EvidenceError, "cryptographic validation"):
            self.verify._bind_proof_to_windows_release_bundle(manifest, no_crypto)

        with tempfile.TemporaryDirectory() as temporary:
            proof_root = Path(temporary)
            authority_path = proof_root / self.verify.WINDOWS_RELEASE_AUTHORITY_NAME
            self.supervisor.atomic_write_json(authority_path, authority)
            binding = {
                "path": self.verify.WINDOWS_RELEASE_AUTHORITY_NAME,
                "sha256": self.supervisor.sha256_file(authority_path),
                "bytes": authority_path.stat().st_size,
            }
            observed = self.verify._validate_windows_release_authority_binding(
                binding,
                proof_root=proof_root,
                manifest=manifest,
                expected_sha=sha,
                eligible=True,
                profile=profile,
            )
            self.assertEqual(observed, authority)

            draft_only = {**authority, "certificationReady": False}
            self.supervisor.atomic_write_json(authority_path, draft_only)
            draft_binding = {
                **binding,
                "sha256": self.supervisor.sha256_file(authority_path),
                "bytes": authority_path.stat().st_size,
            }
            with self.assertRaisesRegex(self.verify.EvidenceError, "draft-only"):
                self.verify._validate_windows_release_authority_binding(
                    draft_binding,
                    proof_root=proof_root,
                    manifest=manifest,
                    expected_sha=sha,
                    eligible=True,
                    profile=profile,
                )
            # Diagnostic/non-certifying runs may retain a valid draft candidate without turning it
            # into a product claim.
            self.verify._validate_windows_release_authority_binding(
                draft_binding,
                proof_root=proof_root,
                manifest=manifest,
                expected_sha=sha,
                eligible=False,
                profile=profile,
            )

            authority_path.write_bytes(authority_path.read_bytes() + b"tamper")
            with self.assertRaisesRegex(self.verify.EvidenceError, "missing or changed"):
                self.verify._validate_windows_release_authority_binding(
                    draft_binding,
                    proof_root=proof_root,
                    manifest=manifest,
                    expected_sha=sha,
                    eligible=False,
                    profile=profile,
                )

    def test_latest_release_freshness_ignores_static_bundle_metadata_but_not_live_drift(self) -> None:
        live = {
            "role": "application-executable",
            "name": "cortex-speech-app.exe",
            "sha256": "a" * 64,
            "bytes": 42,
            "buildGitSha": "b" * 40,
            "matchesFullGitSha": True,
            "authority": "active-immutable-release",
            "activeReleasePointerSha256": "c" * 64,
            "activeReleaseGitSha": "b" * 40,
        }
        recorded = {**live, "path": "cortex-speech-app.exe", "authenticode": {"status": "Valid"}}
        with mock.patch.object(self.verify, "_release_artifact_bindings", return_value=[live]):
            self.verify._revalidate_latest_release_executable(
                self.verify.PROFILE_WINDOWS, [recorded], "b" * 40
            )
        drifted = {**live, "sha256": "d" * 64}
        with (
            mock.patch.object(self.verify, "_release_artifact_bindings", return_value=[drifted]),
            self.assertRaisesRegex(self.verify.EvidenceError, "changed after measurement"),
        ):
            self.verify._revalidate_latest_release_executable(
                self.verify.PROFILE_WINDOWS, [recorded], "b" * 40
            )

    def test_gate_environments_are_allowlisted_and_secret_isolation_is_per_gate(self) -> None:
        sentinel = "CORTEX_AUDIT_SENTINEL_SECRET"
        injected = {
            sentinel: "must-not-reach-a-child",
            "GH_TOKEN": "branch-only-token",
            "CORTEX_DB": str(REPO_ROOT / "healthy-clone.db"),
            "CORTEX_APP_DATA_DIR": str(REPO_ROOT / "substituted-app-data"),
            "CORTEX_APP_EXE": str(REPO_ROOT / "diagnostic-app.exe"),
            "CORTEX_REQUIRE_7B": "0",
        }
        with tempfile.TemporaryDirectory() as temporary, mock.patch.dict(
            os.environ, injected, clear=False
        ), mock.patch.object(
            self.verify,
            "_canonical_live_data_roots",
            return_value=(Path(temporary) / "roaming", Path(temporary) / "local"),
        ):
            for gate in self.verify.GATES:
                child = self.verify._gate_environment(gate)
                self.assertNotIn(sentinel, child, gate.id)
                self.assertEqual(child.get("CORTEX_GATE"), "1", gate.id)
                if gate.id == "branch-protection":
                    self.assertEqual(child.get("GH_TOKEN"), "branch-only-token")
                else:
                    self.assertNotIn("GH_TOKEN", child, gate.id)
                if gate.id in self.verify.LIVE_AUTHORITY_GATE_IDS:
                    self.assertFalse(
                        set(child) & set(self.verify.LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT),
                        gate.id,
                    )
                if gate.id == "champion-7b-preflight":
                    self.assertEqual(child.get("CORTEX_REQUIRE_7B"), "1")
                else:
                    self.assertNotIn("CORTEX_REQUIRE_7B", child, gate.id)
            disposable = self.verify._gate_environment(
                self.verify._gate_by_id("real-app-e2e")
            )
            self.assertEqual(disposable["CORTEX_APP_EXE"], injected["CORTEX_APP_EXE"])

    def test_live_authority_rejects_healthy_clone_substitution_or_marks_it_diagnostic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            actual_roaming = root / "actual-roaming"
            actual_local = root / "actual-local"
            actual_db = actual_roaming / "cortex-speech" / "cortex-speech.db"
            healthy_clone = root / "healthy-clone.db"
            fake_roaming = root / "fake-roaming"
            fake_local = root / "fake-local"
            actual_db.parent.mkdir(parents=True)
            fake_roaming.mkdir()
            fake_local.mkdir()
            actual_local.mkdir()
            actual_db.write_bytes(b"invalid-live-state")
            healthy_clone.write_bytes(b"healthy-clone")
            caller = {
                "APPDATA": str(fake_roaming),
                "LOCALAPPDATA": str(fake_local),
                "CORTEX_DB": str(healthy_clone),
                "CORTEX_DB_DIR": "",
                "CORTEX_APP_DATA_DIR": str(fake_roaming / "cortex-speech"),
            }
            resolver = (
                "import os, pathlib, sys; "
                "target = pathlib.Path(os.environ.get('CORTEX_DB') or "
                "(pathlib.Path(os.environ['APPDATA']) / 'cortex-speech' / 'cortex-speech.db')); "
                "sys.exit(0 if target.read_bytes() == b'healthy-clone' else 19)"
            )
            gate = self.verify._gate_by_id("review-serving-provenance")
            with mock.patch.dict(os.environ, caller, clear=False), mock.patch.object(
                self.verify,
                "_canonical_live_data_roots",
                return_value=(actual_roaming, actual_local),
            ):
                live_authority = self.verify._run_authority_document(
                    diagnostic_overrides=False
                )
                live_mode, live_digest = self.verify._validate_run_authority(live_authority)
                boolean_schema_authority = json.loads(json.dumps(live_authority))
                boolean_schema_authority["schema"] = True
                boolean_schema_authority["authorityDigest"] = self.verify._document_digest(
                    {
                        key: value
                        for key, value in boolean_schema_authority.items()
                        if key != "authorityDigest"
                    }
                )
                with self.assertRaisesRegex(self.verify.EvidenceError, "wrong schema"):
                    self.verify._validate_run_authority(boolean_schema_authority)
                live_environment = self.verify._gate_environment(gate, live_mode)
                self.assertNotIn("CORTEX_DB", live_environment)
                self.assertEqual(Path(live_environment["APPDATA"]), actual_roaming)
                live_attempt = subprocess.run(
                    [sys.executable, "-c", resolver],
                    env=live_environment,
                    check=False,
                )
                self.assertEqual(live_attempt.returncode, 19)
                live_binding = self.verify._gate_environment_authority(
                    gate,
                    live_environment,
                    authority_mode=live_mode,
                    run_authority_digest=live_digest,
                )
                self.verify._validate_gate_environment_authority(
                    live_binding,
                    gate,
                    authority_mode=live_mode,
                    run_authority_digest=live_digest,
                )
                live_bindings = {
                    item["name"]: item for item in live_binding["effectiveEnvironment"]
                }
                self.assertEqual(
                    live_bindings["APPDATA"]["pathSha256"],
                    live_authority["roots"]["roamingAppData"]["absolutePathSha256"],
                )
                forged_binding = json.loads(json.dumps(live_binding))
                forged_binding["effectiveEnvironment"].append(
                    {
                        "name": "CORTEX_DB",
                        "pathSha256": self.verify._redacted_path_digest(healthy_clone),
                    }
                )
                forged_binding["effectiveEnvironment"].sort(key=lambda item: item["name"])
                unsigned_binding = {
                    key: value
                    for key, value in forged_binding.items()
                    if key != "environmentDigest"
                }
                forged_binding["environmentDigest"] = self.verify._document_digest(
                    unsigned_binding
                )
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "allowlist|caller authority"
                ):
                    self.verify._validate_gate_environment_authority(
                        forged_binding,
                        gate,
                        authority_mode=live_mode,
                        run_authority_digest=live_digest,
                    )

                diagnostic_authority = self.verify._run_authority_document(
                    diagnostic_overrides=True
                )
                diagnostic_mode, diagnostic_digest = self.verify._validate_run_authority(
                    diagnostic_authority
                )
                diagnostic_environment = self.verify._gate_environment(gate, diagnostic_mode)
                self.assertEqual(diagnostic_environment["CORTEX_DB"], str(healthy_clone))
                diagnostic_attempt = subprocess.run(
                    [sys.executable, "-c", resolver],
                    env=diagnostic_environment,
                    check=False,
                )
                self.assertEqual(diagnostic_attempt.returncode, 0)
                diagnostic_binding = self.verify._gate_environment_authority(
                    gate,
                    diagnostic_environment,
                    authority_mode=diagnostic_mode,
                    run_authority_digest=diagnostic_digest,
                )
                self.verify._validate_gate_environment_authority(
                    diagnostic_binding,
                    gate,
                    authority_mode=diagnostic_mode,
                    run_authority_digest=diagnostic_digest,
                )
                prepared_live_authority = self.verify._prepare_run_authority(False)
                self.assertIn(
                    "CORTEX_DB", prepared_live_authority["callerOverrides"]["names"]
                )
                self.assertIn(
                    "CORTEX_DB_DIR", prepared_live_authority["callerOverrides"]["names"]
                )
                self.assertNotIn("CORTEX_DB", os.environ)
                self.assertNotIn("CORTEX_APP_DATA_DIR", os.environ)
                self.assertEqual(Path(os.environ["APPDATA"]), actual_roaming)

            verified_evidence = [
                {
                    "classId": spec.id,
                    "status": "VERIFIED",
                    "detail": "synthetic class-specific validator evidence",
                }
                for spec in self.verify._required_evidence_specs(self.verify.PROFILE_OWNER)
            ]
            code, verdict = self.verify._profile_verdict(
                self.verify.PROFILE_OWNER,
                False,
                [(gate.id, self.verify.PASS, 0.1, "")],
                verified_evidence,
                diagnostic_authority_overrides=True,
            )
            self.assertEqual(code, 2)
            self.assertIn("permanently diagnostic", verdict)
            self.assertNotIn("10/10", verdict)
            red_code, red_verdict = self.verify._profile_verdict(
                self.verify.PROFILE_OWNER,
                False,
                [(gate.id, self.verify.FAIL, 0.1, "invalid live authority")],
                verified_evidence,
                diagnostic_authority_overrides=True,
            )
            self.assertEqual(red_code, 1)
            self.assertIn("DIAGNOSTIC", red_verdict)
            self.assertIn("cannot certify", red_verdict)

    def test_diagnostic_live_authority_is_immutable_manifest_state_and_never_certifies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_roaming = root / "caller-roaming"
            fake_local = root / "caller-local"
            fake_roaming.mkdir()
            fake_local.mkdir()
            fixed_checkout_digest = self.verify._checkout_state_digest()
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
                self.verify._assert_source_state,
                self.verify._checkout_state_digest,
            )
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = []
                self.verify._assert_source_state = lambda *_args: None
                self.verify._checkout_state_digest = lambda: fixed_checkout_digest
                with mock.patch.dict(
                    os.environ,
                    {
                        "APPDATA": str(fake_roaming),
                        "LOCALAPPDATA": str(fake_local),
                        "CORTEX_DB": str(root / "healthy-clone.db"),
                    },
                    clear=False,
                ), mock.patch.object(
                    self.verify,
                    "_consume_rust_coverage_prerequisite",
                    side_effect=self._synthetic_coverage_binding,
                ), mock.patch.object(
                    self.verify,
                    "_release_artifact_bindings",
                    side_effect=self._stable_active_release_binding,
                ):
                    code = self.verify.aggregate_main(
                        quick=False,
                        status_md=None,
                        profile=self.verify.PROFILE_OWNER,
                        diagnostic_live_authority_overrides=True,
                    )
                self.assertEqual(code, 2)
                pointer = json.loads(self.verify.LATEST_PROOF.read_text(encoding="utf-8"))
                manifest_path = (self.verify.LATEST_PROOF.parent / pointer["manifest"]).resolve()
                manifest = self.verify._validate_completed_manifest(
                    manifest_path,
                    pointer["fullGitSha"],
                    pointer["runToken"],
                )
                self.assertFalse(manifest["certificationEligible"])
                self.assertEqual(
                    manifest["runAuthority"]["mode"],
                    self.verify.AUTHORITY_MODE_DIAGNOSTIC,
                )
                self.assertIn("CORTEX_DB", manifest["runAuthority"]["callerOverrides"]["names"])
                self.assertIn("permanently diagnostic", manifest["verdict"])
                stored_authority = json.loads(
                    (manifest_path.parent / self.verify.RUN_AUTHORITY_NAME).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(stored_authority, manifest["runAuthority"])
                attestation = json.loads(
                    (manifest_path.parent / self.verify.PRODUCT_ATTESTATION_NAME).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(attestation["runAuthority"], manifest["runAuthority"])
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "not certification-eligible"
                ):
                    self.verify._require_certifying_manifest(
                        manifest,
                        self.verify.PROFILE_OWNER,
                    )
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                    self.verify._assert_source_state,
                    self.verify._checkout_state_digest,
                ) = original

    def test_live_lease_refuses_a_concurrent_start(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            first = self.supervisor.LeaseManager(lease, "a" * 40, "owner-product", "first")
            second = self.supervisor.LeaseManager(lease, "a" * 40, "owner-product", "second")
            first.acquire()
            try:
                with self.assertRaisesRegex(self.supervisor.LeaseError, "is live"):
                    second.acquire()
            finally:
                first.release()

    def test_dead_legacy_lock_self_heals_but_the_recovery_invocation_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            original = self.verify.LEGACY_RUN_LOCK
            try:
                self.verify.LEGACY_RUN_LOCK = Path(temporary) / "legacy.lock"
                self.verify.LEGACY_RUN_LOCK.write_text("11456\n", encoding="utf-8")
                with mock.patch.object(self.verify, "_pid_alive", return_value=False):
                    with self.assertRaisesRegex(
                        self.verify.LeaseError,
                        "recovery invocation cannot certify",
                    ):
                        self.verify._retire_legacy_run_lock()
                self.assertFalse(self.verify.LEGACY_RUN_LOCK.exists())
                # The next invocation has no recovery event and may proceed to the typed lease.
                self.verify._retire_legacy_run_lock()
            finally:
                self.verify.LEGACY_RUN_LOCK = original

    def test_live_or_pid_reused_legacy_lock_is_never_terminated_or_removed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            original = self.verify.LEGACY_RUN_LOCK
            try:
                self.verify.LEGACY_RUN_LOCK = Path(temporary) / "legacy.lock"
                self.verify.LEGACY_RUN_LOCK.write_text("24680\n", encoding="utf-8")
                with mock.patch.object(self.verify, "_pid_alive", return_value=True):
                    with self.assertRaisesRegex(
                        self.verify.LeaseError,
                        "creation-time/token identity",
                    ):
                        self.verify._retire_legacy_run_lock()
                self.assertEqual(
                    self.verify.LEGACY_RUN_LOCK.read_text(encoding="utf-8"),
                    "24680\n",
                )
            finally:
                self.verify.LEGACY_RUN_LOCK = original

    def test_malformed_legacy_lock_fails_closed_without_deletion(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            original = self.verify.LEGACY_RUN_LOCK
            try:
                self.verify.LEGACY_RUN_LOCK = Path(temporary) / "legacy.lock"
                self.verify.LEGACY_RUN_LOCK.write_text("11456 extra\n", encoding="utf-8")
                with self.assertRaisesRegex(
                    self.verify.LeaseError,
                    "unknown legacy verifier lock identity",
                ):
                    self.verify._retire_legacy_run_lock()
                self.assertTrue(self.verify.LEGACY_RUN_LOCK.exists())
            finally:
                self.verify.LEGACY_RUN_LOCK = original

    def test_two_concurrent_starts_have_exactly_one_winner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            barrier = threading.Barrier(2)
            release = threading.Event()
            outcomes: list[str] = []

            def contender(token: str) -> None:
                manager = self.supervisor.LeaseManager(lease, "b" * 40, "owner-product", token)
                barrier.wait()
                try:
                    manager.acquire()
                except self.supervisor.LeaseError:
                    outcomes.append("refused")
                    return
                outcomes.append("acquired")
                release.wait(5)
                manager.release()

            threads = [threading.Thread(target=contender, args=(token,)) for token in ("one", "two")]
            for thread in threads:
                thread.start()
            deadline = time.monotonic() + 5
            while len(outcomes) < 2 and time.monotonic() < deadline:
                time.sleep(0.02)
            release.set()
            for thread in threads:
                thread.join(timeout=5)
            self.assertCountEqual(outcomes, ["acquired", "refused"])

    def test_killed_contender_leaves_an_identity_bound_takeover_that_is_recovered(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            takeover = lease.with_suffix(lease.suffix + ".takeover")
            lease.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": "dead-holder",
                        "pid": 2_000_000_000,
                        "processCreationTime": "gone",
                        "heartbeatUnix": time.time() - 120,
                    }
                ),
                encoding="utf-8",
            )
            contender = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(120)"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            contender_creation = self.supervisor.process_creation_time(contender.pid)
            self.assertIsNotNone(contender_creation)
            takeover.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": "killed-contender",
                        "pid": contender.pid,
                        "processCreationTime": contender_creation,
                        "startedUnix": time.time(),
                        "startedMonotonic": time.monotonic(),
                    }
                ),
                encoding="utf-8",
            )
            contender.kill()
            contender.wait(timeout=5)

            replacement = self.supervisor.LeaseManager(
                lease, "b" * 40, "owner-product", "replacement"
            )
            self.assertEqual(replacement.acquire(), "dead-holder")
            self.assertFalse(takeover.exists())
            replacement.release()

    def test_a_refreshed_heartbeat_wins_the_atomic_final_takeover_check(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            holder = self.supervisor.LeaseManager(lease, "b" * 40, "owner-product", "holder")
            holder.acquire()
            stale = json.loads(lease.read_text(encoding="utf-8"))
            stale["heartbeatUnix"] = time.time() - 120
            stale["heartbeatMonotonic"] = time.monotonic() - 120
            lease.write_text(json.dumps(stale), encoding="utf-8")
            contender = self.supervisor.LeaseManager(
                lease, "b" * 40, "owner-product", "contender"
            )
            try:
                with mock.patch.object(
                    self.supervisor,
                    "_before_takeover_final_recheck",
                    side_effect=lambda: holder.heartbeat(force=True),
                ), mock.patch.object(
                    self.supervisor,
                    "_terminate_verified_process_tree",
                    side_effect=AssertionError("a refreshed holder must never be terminated"),
                ):
                    with self.assertRaisesRegex(self.supervisor.LeaseError, "refreshed its heartbeat"):
                        contender.acquire()
                current = json.loads(lease.read_text(encoding="utf-8"))
                self.assertEqual(current["runToken"], "holder")
                self.assertFalse(lease.with_suffix(lease.suffix + ".takeover").exists())
            finally:
                holder.release()

    def test_monotonic_heartbeat_prevents_a_wall_clock_jump_takeover(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            holder = self.supervisor.LeaseManager(lease, "b" * 40, "owner-product", "holder")
            holder.acquire()
            jumped = json.loads(lease.read_text(encoding="utf-8"))
            jumped["heartbeatUnix"] = time.time() - 24 * 60 * 60
            jumped["heartbeatMonotonic"] = time.monotonic()
            lease.write_text(json.dumps(jumped), encoding="utf-8")
            contender = self.supervisor.LeaseManager(
                lease, "b" * 40, "owner-product", "contender"
            )
            try:
                with mock.patch.object(
                    self.supervisor,
                    "_terminate_verified_process_tree",
                    side_effect=AssertionError("wall-clock movement must not kill a fresh holder"),
                ):
                    with self.assertRaisesRegex(self.supervisor.LeaseError, "is live"):
                        contender.acquire()
            finally:
                holder.release()

    def test_dead_holder_is_replaced_but_pid_reuse_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            dead_token = "dead-holder"
            lease.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": dead_token,
                        "pid": 2_000_000_000,
                        "processCreationTime": "gone",
                        "heartbeatUnix": time.time() - 120,
                    }
                ),
                encoding="utf-8",
            )
            replacement = self.supervisor.LeaseManager(
                lease, "c" * 40, "owner-product", "replacement"
            )
            self.assertEqual(replacement.acquire(), dead_token)
            replacement.release()

            lease.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": "reused",
                        "pid": os.getpid(),
                        "processCreationTime": "not-this-process",
                        "heartbeatUnix": time.time() - 120,
                    }
                ),
                encoding="utf-8",
            )
            refused = self.supervisor.LeaseManager(lease, "c" * 40, "owner-product", "new")
            with self.assertRaisesRegex(self.supervisor.LeaseError, "PID was reused"):
                refused.acquire()

    def test_verified_wedged_holder_is_terminated_and_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            holder = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(120)"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0,
                start_new_session=os.name != "nt",
            )
            try:
                creation = self.supervisor.process_creation_time(holder.pid)
                self.assertIsNotNone(creation)
                lease.write_text(
                    json.dumps(
                        {
                            "schema": 1,
                            "runToken": "wedged",
                            "pid": holder.pid,
                            "processCreationTime": creation,
                            "heartbeatUnix": time.time() - 120,
                        }
                    ),
                    encoding="utf-8",
                )
                replacement = self.supervisor.LeaseManager(
                    lease, "d" * 40, "owner-product", "replacement"
                )
                started = time.monotonic()
                self.assertEqual(replacement.acquire(), "wedged")
                self.assertLess(time.monotonic() - started, 60)
                holder.wait(timeout=5)
                replacement.release()
            finally:
                if holder.poll() is None:
                    holder.kill()
                    holder.wait(timeout=5)

    @unittest.skipUnless(os.name == "nt", "Windows suspended-process ordering")
    def test_job_is_constructed_before_suspended_process_assignment_and_resume(self) -> None:
        events: list[str] = []

        class FakeProcess:
            pid = 424242
            _handle = 31337

            def poll(self):
                return None

        class RecordingJob:
            def __init__(self) -> None:
                events.append("job-created")

            def assign(self, process) -> None:
                self.assert_suspended_process(process)
                events.append("assigned")

            @staticmethod
            def assert_suspended_process(process) -> None:
                if process.pid != FakeProcess.pid:
                    raise AssertionError("wrong process assigned")

            def resume(self, process) -> None:
                self.assert_suspended_process(process)
                events.append("resumed")

            def close(self) -> None:
                events.append("closed")

        def fake_popen(*args, **kwargs):
            self.assertTrue(kwargs["creationflags"] & 0x00000004)
            events.append("process-created-suspended")
            return FakeProcess()

        with mock.patch.object(self.supervisor, "WindowsJob", RecordingJob), mock.patch.object(
            self.supervisor.subprocess, "Popen", side_effect=fake_popen
        ), mock.patch.object(self.supervisor, "process_creation_time", return_value="created"):
            process, _job = self.supervisor.spawn_isolated(
                [sys.executable, "-c", "pass"], cwd=REPO_ROOT, log=io.StringIO()
            )
        self.assertEqual(process.pid, FakeProcess.pid)
        self.assertEqual(
            events,
            ["job-created", "process-created-suspended", "assigned", "resumed"],
        )

    @unittest.skipUnless(os.name == "nt", "Windows suspended-process fault cleanup")
    def test_constructor_assignment_and_resume_failures_leave_no_worker(self) -> None:
        class ConstructorFaultKernel:
            def __init__(self) -> None:
                self.closed: list[int] = []

            @staticmethod
            def CreateJobObjectW(_security, _name):
                return 1234

            @staticmethod
            def SetInformationJobObject(_handle, _kind, _info, _size):
                return False

            def CloseHandle(self, handle):
                self.closed.append(int(handle))
                return True

        constructor_kernel = ConstructorFaultKernel()
        with mock.patch.object(
            self.supervisor, "_windows_kernel32", return_value=constructor_kernel
        ):
            with self.assertRaisesRegex(OSError, "SetInformationJobObject failed"):
                self.supervisor.WindowsJob()
        self.assertEqual(constructor_kernel.closed, [1234])

        with mock.patch.object(
            self.supervisor, "WindowsJob", side_effect=OSError("job constructor fault")
        ), mock.patch.object(self.supervisor.subprocess, "Popen") as popen:
            with self.assertRaisesRegex(OSError, "constructor fault"):
                self.supervisor.spawn_isolated(
                    [sys.executable, "-c", "pass"], cwd=REPO_ROOT, log=io.StringIO()
                )
            popen.assert_not_called()

        constructed_jobs: list[object] = []

        class TrackingJob:
            def __init__(self) -> None:
                self.closed = False
                constructed_jobs.append(self)

            def close(self) -> None:
                self.closed = True

        with mock.patch.object(self.supervisor, "WindowsJob", TrackingJob), mock.patch.object(
            self.supervisor.subprocess, "Popen", side_effect=OSError("CreateProcess fault")
        ):
            with self.assertRaisesRegex(OSError, "CreateProcess fault"):
                self.supervisor.spawn_isolated(
                    [sys.executable, "-c", "pass"], cwd=REPO_ROOT, log=io.StringIO()
                )
        self.assertEqual(len(constructed_jobs), 1)
        self.assertTrue(constructed_jobs[0].closed)

        for fault in ("assign", "resume"):
            captured: dict[str, object] = {}
            real_job_type = self.supervisor.WindowsJob
            fault_jobs: list[object] = []

            class FaultJob:
                def __init__(self) -> None:
                    self.inner = real_job_type()
                    fault_jobs.append(self)

                def assign(self, process) -> None:
                    captured["pid"] = process.pid
                    captured["creation"] = self.supervisor_creation(process.pid)
                    if fault == "assign":
                        raise OSError("injected assignment failure")
                    self.inner.assign(process)

                @staticmethod
                def supervisor_creation(pid: int):
                    return Verify10SupervisorTests.supervisor.process_creation_time(pid)

                def resume(self, process) -> None:
                    if fault == "resume":
                        raise OSError("injected resume failure")

                def terminate(self, exit_code: int = 1) -> None:
                    self.inner.terminate(exit_code)

                def close(self) -> None:
                    self.inner.close()

            with tempfile.TemporaryDirectory() as temporary:
                log_path = Path(temporary) / f"{fault}.log"
                with log_path.open("w", encoding="utf-8") as log, mock.patch.object(
                    self.supervisor, "WindowsJob", FaultJob
                ):
                    with self.assertRaisesRegex(OSError, f"injected {fault}"):
                        self.supervisor.spawn_isolated(
                            [sys.executable, "-c", "import time; time.sleep(120)"],
                            cwd=Path(temporary),
                            log=log,
                        )
            pid = int(captured["pid"])
            self.assertIsNotNone(captured["creation"])
            creation = str(captured["creation"])
            self.assertNotEqual(
                self.supervisor.process_creation_time(pid),
                creation,
                f"{fault} fault leaked suspended worker {pid}",
            )
            self.assertEqual(len(fault_jobs), 1)
            self.assertIsNone(fault_jobs[0].inner.handle)

    @unittest.skipUnless(os.name == "nt", "Windows Job inheritance proof")
    def test_parent_exit_after_immediate_grandchild_still_leaves_no_survivor(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            grandchild_file = root / "grandchild.json"
            script = (
                "import json,subprocess,sys;"
                "g=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)']);"
                f"open(r'{grandchild_file}','w').write(json.dumps(g.pid))"
            )
            job = None
            grandchild_pid = None
            grandchild_creation = None
            try:
                with open(os.devnull, "w", encoding="utf-8") as log:
                    process, job = self.supervisor.spawn_isolated(
                        [sys.executable, "-c", script], cwd=root, log=log
                    )
                    process.wait(timeout=10)
                    deadline = time.monotonic() + 5
                    while not grandchild_file.exists() and time.monotonic() < deadline:
                        time.sleep(0.02)
                    self.assertTrue(grandchild_file.exists())
                    grandchild_pid = int(json.loads(grandchild_file.read_text(encoding="utf-8")))
                    grandchild_creation = self.supervisor.process_creation_time(grandchild_pid)
                    self.assertIsNotNone(grandchild_creation)
                    job.close()
                    job = None
                deadline = time.monotonic() + 5
                while (
                    self.supervisor.process_creation_time(grandchild_pid) == grandchild_creation
                    and time.monotonic() < deadline
                ):
                    time.sleep(0.05)
                self.assertNotEqual(
                    self.supervisor.process_creation_time(grandchild_pid),
                    grandchild_creation,
                )
            finally:
                if job is not None:
                    try:
                        job.close()
                    except OSError:
                        pass
                if (
                    grandchild_pid is not None
                    and grandchild_creation is not None
                    and self.supervisor.process_creation_time(grandchild_pid) == grandchild_creation
                ):
                    subprocess.run(
                        ["taskkill", "/PID", str(grandchild_pid), "/T", "/F"],
                        stdin=subprocess.DEVNULL,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        check=False,
                        timeout=10,
                    )

    def test_timeout_kills_hanging_child_and_grandchild(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pids = root / "pids.json"
            pids_staging = root / "pids.json.staging"
            log_path = root / "worker.log"
            script = (
                "import json,os,subprocess,sys,time;"
                "g=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)']);"
                f"f=open(r'{pids_staging}','w',encoding='utf-8');"
                "f.write(json.dumps([os.getpid(),g.pid]));f.flush();os.fsync(f.fileno());f.close();"
                f"os.replace(r'{pids_staging}',r'{pids}');"
                "time.sleep(120)"
            )
            process = None
            job = None
            identities: list[tuple[int, str | None]] = []
            timed_out = False
            with log_path.open("w", encoding="utf-8") as log:
                try:
                    process, job = self.supervisor.spawn_isolated(
                        [sys.executable, "-c", script], cwd=root, log=log
                    )
                    deadline = time.monotonic() + 5
                    published_pids = None
                    last_publication_error = "PID publication did not appear"
                    while time.monotonic() < deadline:
                        try:
                            candidate = json.loads(pids.read_text(encoding="utf-8"))
                            if (
                                isinstance(candidate, list)
                                and len(candidate) == 2
                                and all(isinstance(pid, int) and pid > 0 for pid in candidate)
                            ):
                                published_pids = candidate
                                break
                            last_publication_error = f"invalid PID publication: {candidate!r}"
                        except (OSError, json.JSONDecodeError) as error:
                            last_publication_error = str(error)
                        time.sleep(0.02)
                    self.assertIsNotNone(
                        published_pids,
                        f"worker did not atomically publish both process identities: {last_publication_error}",
                    )
                    identities = [
                        (pid, self.supervisor.process_creation_time(pid))
                        for pid in published_pids or []
                    ]
                    self.assertTrue(all(creation is not None for _pid, creation in identities))
                    _return_code, timed_out = self.supervisor.wait_isolated(
                        process, job, timeout=0.2, heartbeat=lambda: None
                    )
                    job = None  # wait_isolated closed the kill-on-close authority.
                finally:
                    if job is not None and process is not None:
                        self.supervisor.terminate_isolated(process, job)
            self.assertTrue(timed_out)
            deadline = time.monotonic() + 5
            while any(
                creation is not None and self.supervisor.process_creation_time(pid) == creation
                for pid, creation in identities
            ) and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(
                all(
                    creation is None or self.supervisor.process_creation_time(pid) != creation
                    for pid, creation in identities
                )
            )

    @_requires_windows_live_authority
    def test_keyboard_interrupt_terminates_worker_and_publishes_no_status(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lease = self.supervisor.LeaseManager(
                root / "lease.json", "e" * 40, "owner-product", "interrupt"
            )
            lease.acquire()
            journal = self.supervisor.EvidenceJournal(root / "events.jsonl", "interrupt")
            gate = self.verify._gate_by_id("manifest-alignment")
            authority = self.verify._run_authority_document(diagnostic_overrides=False)
            authority_mode, authority_digest = self.verify._validate_run_authority(authority)

            class FakeProcess:
                pid = 424_242

            fake_process = FakeProcess()
            fake_job = object()
            try:
                with mock.patch.object(
                    self.verify,
                    "spawn_isolated",
                    return_value=(fake_process, fake_job),
                ), mock.patch.object(
                    self.verify,
                    "wait_isolated",
                    side_effect=KeyboardInterrupt,
                ), mock.patch.object(
                    self.verify,
                    "terminate_isolated",
                ) as terminate:
                    with self.assertRaises(KeyboardInterrupt):
                        self.verify._run_gate_worker(
                            gate,
                            root / "run",
                            "interrupt",
                            lease,
                            journal,
                            profile=self.verify.PROFILE_OWNER,
                            authority_mode=authority_mode,
                            run_authority_digest=authority_digest,
                        )
                terminate.assert_called_once_with(fake_process, fake_job)
                events = [
                    json.loads(line)
                    for line in (root / "events.jsonl").read_text(encoding="utf-8").splitlines()
                ]
                self.assertEqual(events[-1]["event"], "gate_end")
                self.assertEqual(events[-1]["status"], "ABORTED")
                self.assertEqual(events[-1]["reason"], "KeyboardInterrupt")
                self.assertFalse((root / "latest-proof.json").exists())
            finally:
                lease.release()

    @_requires_windows_live_authority
    def test_declared_gate_timeout_is_the_exact_parent_hard_limit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lease = self.supervisor.LeaseManager(
                root / "lease.json", "e" * 40, "owner-product", "timeout"
            )
            lease.acquire()
            journal = self.supervisor.EvidenceJournal(root / "events.jsonl", "timeout")
            gate = replace(
                self.verify._gate_by_id("manifest-alignment"), timeout_seconds=7
            )
            authority = self.verify._run_authority_document(diagnostic_overrides=False)
            authority_mode, authority_digest = self.verify._validate_run_authority(authority)

            class FakeProcess:
                pid = 424_243

            observed_timeouts: list[float] = []

            def timed_out_wait(_process, _job, *, timeout, heartbeat):
                observed_timeouts.append(timeout)
                heartbeat()
                return None, True

            try:
                with mock.patch.object(
                    self.verify,
                    "spawn_isolated",
                    return_value=(FakeProcess(), object()),
                ), mock.patch.object(
                    self.verify,
                    "wait_isolated",
                    side_effect=timed_out_wait,
                ):
                    (
                        status,
                        seconds,
                        detail,
                        _artifacts,
                        _environment_authority,
                        attempt_authority,
                    ) = self.verify._run_gate_worker(
                        gate,
                        root / "run",
                        "timeout",
                        lease,
                        journal,
                        profile=self.verify.PROFILE_OWNER,
                        authority_mode=authority_mode,
                        run_authority_digest=authority_digest,
                    )
            finally:
                lease.release()
            self.assertEqual(observed_timeouts, [7])
            self.assertEqual(status, self.verify.FAIL)
            self.assertEqual(seconds, 7.0)
            self.assertIn("declared hard timeout 7s", detail)
            self.assertEqual(
                attempt_authority,
                {"attemptCount": 1, "retryCount": 0, "retryReasons": []},
            )

    @unittest.skipUnless(os.name == "nt", "real Windows console interrupt proof")
    def test_real_console_interrupt_terminates_worker_and_publishes_no_status(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ready = root / "ready.json"
            ready_staging = root / "ready.json.staging"
            lease_path = root / "lease.json"
            event_path = root / "events.jsonl"
            pointer = root / "latest-proof.json"
            worker_log = root / "worker.log"
            helper_script = (
                "import importlib.util,json,os,pathlib,signal,sys,time;"
                f"p=pathlib.Path(r'{SUPERVISOR}');"
                "s=importlib.util.spec_from_file_location('ctrl_c_supervisor',p);"
                "m=importlib.util.module_from_spec(s);sys.modules[s.name]=m;s.loader.exec_module(m);"
                f"root=pathlib.Path(r'{root}');"
                f"lease=m.LeaseManager(pathlib.Path(r'{lease_path}'),'a'*40,'owner-product','ctrl-c');"
                f"journal=m.EvidenceJournal(pathlib.Path(r'{event_path}'),'ctrl-c');"
                "signal.signal(signal.SIGBREAK,lambda *_args:(_ for _ in ()).throw(KeyboardInterrupt()));"
                "lease.acquire();process=None;job=None;"
                "\ntry:\n"
                f" log=open(r'{worker_log}','w',encoding='utf-8')\n"
                " process,job=m.spawn_isolated([sys.executable,'-c','import time;time.sleep(120)'],cwd=root,log=log)\n"
                " creation=m.process_creation_time(process.pid)\n"
                " lease.update_gate('interrupt-fixture',process.pid)\n"
                f" f=open(r'{ready_staging}','w',encoding='utf-8');f.write(json.dumps([process.pid,creation]));f.flush();os.fsync(f.fileno());f.close();os.replace(r'{ready_staging}',r'{ready}')\n"
                " m.wait_isolated(process,job,timeout=120,heartbeat=lease.heartbeat)\n"
                "except KeyboardInterrupt:\n"
                " if process is not None and job is not None:m.terminate_isolated(process,job)\n"
                " lease.update_gate(None,None)\n"
                " journal.append('gate_end',gate='interrupt-fixture',status='ABORTED',reason='KeyboardInterrupt')\n"
                " raise SystemExit(130)\n"
                "finally:\n"
                " lease.release()\n"
            )
            helper = subprocess.Popen(
                [sys.executable, "-c", helper_script],
                cwd=root,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
            )
            child_pid = None
            child_creation = None
            try:
                deadline = time.monotonic() + 10
                while not ready.exists() and time.monotonic() < deadline:
                    if helper.poll() is not None:
                        break
                    time.sleep(0.02)
                stdout, stderr = ("", "")
                if helper.poll() is not None:
                    stdout, stderr = helper.communicate(timeout=1)
                self.assertTrue(ready.exists(), f"interrupt fixture failed: {stdout} {stderr}")
                child_pid, child_creation = json.loads(ready.read_text(encoding="utf-8"))
                self.assertEqual(
                    self.supervisor.process_creation_time(child_pid), child_creation
                )
                helper.send_signal(signal.CTRL_BREAK_EVENT)
                self.assertEqual(helper.wait(timeout=10), 130)
                events = [
                    json.loads(line)
                    for line in event_path.read_text(encoding="utf-8").splitlines()
                    if line.strip()
                ]
                self.assertEqual(events[-1]["status"], "ABORTED")
                self.assertEqual(events[-1]["reason"], "KeyboardInterrupt")
                self.assertFalse(lease_path.exists())
                self.assertFalse(pointer.exists())
                deadline = time.monotonic() + 5
                while (
                    self.supervisor.process_creation_time(child_pid) == child_creation
                    and time.monotonic() < deadline
                ):
                    time.sleep(0.05)
                self.assertNotEqual(
                    self.supervisor.process_creation_time(child_pid), child_creation
                )
            finally:
                if helper.poll() is None:
                    helper.kill()
                    helper.wait(timeout=5)
                if (
                    child_pid is not None
                    and self.supervisor.process_creation_time(child_pid) == child_creation
                ):
                    subprocess.run(
                        ["taskkill", "/PID", str(child_pid), "/T", "/F"],
                        stdin=subprocess.DEVNULL,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        check=False,
                        timeout=10,
                    )
                if helper.stdout is not None:
                    helper.stdout.close()
                if helper.stderr is not None:
                    helper.stderr.close()

    @unittest.skipUnless(os.name == "nt", "Windows Job parent-kill and handle inheritance proof")
    def test_killed_parent_closes_job_with_inherited_grandchild(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity_path = root / "descendants.json"
            staging_path = root / "descendants.json.staging"
            log_path = root / "worker.log"
            child_script = (
                "import json,os,subprocess,sys,time;"
                "g=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)']);"
                f"f=open(r'{staging_path}','w',encoding='utf-8');"
                "f.write(json.dumps([os.getpid(),g.pid]));f.flush();os.fsync(f.fileno());f.close();"
                f"os.replace(r'{staging_path}',r'{identity_path}');"
                "time.sleep(120)"
            )
            parent_script = (
                "import importlib.util,pathlib,sys,time;"
                f"p=pathlib.Path(r'{SUPERVISOR}');"
                "s=importlib.util.spec_from_file_location('parent_kill_supervisor',p);"
                "m=importlib.util.module_from_spec(s);sys.modules[s.name]=m;s.loader.exec_module(m);"
                f"root=pathlib.Path(r'{root}');log=open(r'{log_path}','w',encoding='utf-8');"
                f"process,job=m.spawn_isolated([sys.executable,'-c',{child_script!r}],cwd=root,log=log);"
                "time.sleep(120)"
            )
            parent = subprocess.Popen(
                [sys.executable, "-c", parent_script],
                cwd=root,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            identities: list[tuple[int, str | None]] = []
            try:
                deadline = time.monotonic() + 10
                while not identity_path.exists() and time.monotonic() < deadline:
                    if parent.poll() is not None:
                        break
                    time.sleep(0.02)
                stderr = parent.stderr.read() if parent.poll() is not None and parent.stderr else ""
                self.assertTrue(identity_path.exists(), f"parent fixture failed: {stderr}")
                descendant_pids = json.loads(identity_path.read_text(encoding="utf-8"))
                self.assertEqual(len(descendant_pids), 2)
                identities = [
                    (pid, self.supervisor.process_creation_time(pid))
                    for pid in descendant_pids
                ]
                self.assertTrue(all(creation is not None for _pid, creation in identities))
                parent.kill()
                parent.wait(timeout=5)
                deadline = time.monotonic() + 5
                while any(
                    creation is not None
                    and self.supervisor.process_creation_time(pid) == creation
                    for pid, creation in identities
                ) and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(
                    all(
                        creation is None
                        or self.supervisor.process_creation_time(pid) != creation
                        for pid, creation in identities
                    ),
                    "killing the Job-owning parent left an assigned descendant alive",
                )
            finally:
                if parent.poll() is None:
                    parent.kill()
                    parent.wait(timeout=5)
                if parent.stderr is not None:
                    parent.stderr.close()
                for pid, creation in identities:
                    if self.supervisor.process_creation_time(pid) == creation:
                        subprocess.run(
                            ["taskkill", "/PID", str(pid), "/T", "/F"],
                            stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            check=False,
                            timeout=10,
                        )

    def test_process_kill_during_pointer_publication_never_exposes_partial_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pointer = root / "latest-proof.json"
            pointer.write_text(json.dumps({"schema": 1, "value": "prior"}), encoding="utf-8")
            marker = root / "public-validation-started"
            counter = root / "validation-count"
            script = (
                "import importlib.util,json,pathlib,sys,time\n"
                f"p=pathlib.Path(r'{SUPERVISOR}')\n"
                "s=importlib.util.spec_from_file_location('publication_kill_supervisor',p)\n"
                "m=importlib.util.module_from_spec(s)\n"
                "sys.modules[s.name]=m\n"
                "s.loader.exec_module(m)\n"
                f"pointer=pathlib.Path(r'{pointer}')\n"
                f"marker=pathlib.Path(r'{marker}')\n"
                f"counter=pathlib.Path(r'{counter}')\n"
                "def validate(path):\n"
                " value=json.loads(path.read_text(encoding='utf-8'))\n"
                " assert value == {'schema':1,'value':'candidate'}\n"
                " count=int(counter.read_text())+1 if counter.exists() else 1\n"
                " counter.write_text(str(count))\n"
                " if count == 2:\n"
                "  marker.write_text('published')\n"
                "  time.sleep(120)\n"
                "m.publish_validated_json(pointer,{'schema':1,'value':'candidate'},validate)\n"
            )
            process = subprocess.Popen(
                [sys.executable, "-c", script],
                cwd=root,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 10
                while not marker.exists() and time.monotonic() < deadline:
                    if process.poll() is not None:
                        break
                    time.sleep(0.02)
                stderr = process.stderr.read() if process.poll() is not None and process.stderr else ""
                self.assertTrue(
                    marker.exists(),
                    f"fixture never reached public-name validation: {stderr}",
                )
                process.kill()
                process.wait(timeout=5)
                self.assertEqual(
                    json.loads(pointer.read_text(encoding="utf-8")),
                    {"schema": 1, "value": "candidate"},
                )
                self.assertEqual(list(root.glob("*.candidate")), [])
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                if process.stderr is not None:
                    process.stderr.close()

    def test_disk_full_and_unwritable_evidence_are_terminal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "evidence.json"
            with mock.patch.object(
                self.supervisor.os,
                "fsync",
                side_effect=OSError(errno.ENOSPC, "injected disk full"),
            ):
                with self.assertRaisesRegex(self.supervisor.EvidenceError, "atomic evidence write failed"):
                    self.supervisor.atomic_write_json(output, {"schema": 1})
            self.assertFalse(output.exists())
            self.assertEqual(list(root.glob("*.tmp")), [])
            with self.assertRaises(self.supervisor.EvidenceError):
                journal = self.supervisor.EvidenceJournal(root, "token")
                journal.append("run_start")

    def test_authority_json_rejects_duplicate_keys_and_nonfinite_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            nonfinite = root / "nonfinite.json"
            with self.assertRaisesRegex(
                self.supervisor.EvidenceError, "not canonical or finite"
            ):
                self.supervisor.atomic_write_json(
                    nonfinite, {"schema": 1, "seconds": float("nan")}
                )
            self.assertFalse(nonfinite.exists())

            duplicate = root / "duplicate.json"
            duplicate.write_text(
                '{"schema":1,"runToken":"first","runToken":"second"}\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(self.supervisor.LeaseError, "duplicate key"):
                self.supervisor.read_json_object(duplicate)
            with self.assertRaisesRegex(self.verify.EvidenceError, "duplicate JSON key"):
                self.verify._load_json_without_duplicate_keys(duplicate)

            journal = self.supervisor.EvidenceJournal(root / "events.jsonl", "token")
            with self.assertRaisesRegex(
                self.supervisor.EvidenceError, "cannot append verifier evidence"
            ):
                journal.append("heartbeat", seconds=float("inf"))

    def test_probe_crash_fails_only_its_gate(self) -> None:
        def crash():
            raise RuntimeError("injected probe crash")

        status, _seconds, detail = self.verify.run_gate(
            "probe-crash-campaign", "fn", lambda: True, None, crash
        )
        self.assertEqual(status, self.verify.FAIL)
        self.assertIn("probe crashed", detail)

    @unittest.skipUnless(
        os.name == "nt",
        "injects an NTSTATUS termination through kernel32 OpenProcess/TerminateProcess (ctypes.WinDLL)",
    )
    def test_abnormal_node_termination_retry_is_noncertifying(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            node = shutil.which("node")
            self.assertIsNotNone(node, "the Windows verifier fault campaign requires Node")
            marker = root / "node.pid"
            script_path = root / "abnormal-exit.js"
            script_path.write_text(
                "const fs=require('fs');\n"
                f"const marker={json.dumps(str(marker))};\n"
                "if (fs.existsSync(marker)) process.exit(0);\n"
                "fs.writeFileSync(marker,String(process.pid));\n"
                "setInterval(()=>{},1000);\n",
                encoding="utf-8",
            )
            original_log_dir = self.verify.LOG_DIR
            self.verify.LOG_DIR = root
            killed_identity: list[tuple[int, str | None]] = []
            killer_errors: list[BaseException] = []

            def terminate_node_with_abnormal_status() -> None:
                try:
                    deadline = time.monotonic() + 10
                    while not marker.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    pid = int(marker.read_text(encoding="utf-8"))
                    creation = self.supervisor.process_creation_time(pid)
                    killed_identity.append((pid, creation))
                    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
                    kernel32.OpenProcess.argtypes = [
                        wintypes.DWORD,
                        wintypes.BOOL,
                        wintypes.DWORD,
                    ]
                    kernel32.OpenProcess.restype = wintypes.HANDLE
                    kernel32.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
                    kernel32.TerminateProcess.restype = wintypes.BOOL
                    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
                    kernel32.CloseHandle.restype = wintypes.BOOL
                    handle = kernel32.OpenProcess(0x0001, False, pid)
                    if not handle:
                        raise OSError(ctypes.get_last_error(), "OpenProcess failed")
                    try:
                        if not kernel32.TerminateProcess(handle, 0xC0000409):
                            raise OSError(ctypes.get_last_error(), "TerminateProcess failed")
                    finally:
                        kernel32.CloseHandle(handle)
                except BaseException as error:  # noqa: BLE001 - delivered to the asserting thread
                    killer_errors.append(error)

            killer = threading.Thread(target=terminate_node_with_abnormal_status, daemon=True)
            killer.start()
            try:
                attempt_authority = self.verify.GateRunMetadata()
                status, _seconds, detail = self.verify.run_gate(
                    "abnormal-node-campaign",
                    "cmd",
                    f'"{node}" "{script_path}"',
                    root,
                    None,
                    timeout=60,
                    metadata=attempt_authority,
                )
                killer.join(timeout=10)
                self.assertFalse(killer.is_alive())
                self.assertEqual(killer_errors, [])
                self.assertEqual(len(killed_identity), 1)
                self.assertEqual(status, self.verify.PASS_AFTER_RETRY)
                self.assertIn("re-ran once", detail)
                self.assertEqual(attempt_authority.attempt_count, 2)
                self.assertEqual(attempt_authority.retry_count, 1)
                self.assertEqual(
                    attempt_authority.retry_reasons,
                    ("OS-terminated before verdict (exit 3221226505)",),
                )
                self.assertEqual(len(list(root.glob("abnormal-node-campaign.attempt-*.log"))), 2)
                code, _verdict = self.verify._profile_verdict(
                    self.verify.PROFILE_OWNER,
                    False,
                    [("abnormal-node-campaign", status, 0.2, detail)],
                    [],
                )
                self.assertNotEqual(code, 0)
            finally:
                self.verify.LOG_DIR = original_log_dir
                killer.join(timeout=1)

    def test_occupied_development_port_fails_without_retry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            node = shutil.which("node")
            self.assertIsNotNone(node, "the Windows verifier fault campaign requires Node")

            class AnsweringDebugPort(BaseHTTPRequestHandler):
                requests = 0

                def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract
                    type(self).requests += 1
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(b"[]")

                def log_message(self, _format: str, *_args: object) -> None:
                    return

            server = None
            for declared_port in (9271, 9261, 9333, 9334, 9335, 9355):
                try:
                    server = ThreadingHTTPServer(("127.0.0.1", declared_port), AnsweringDebugPort)
                    break
                except OSError:
                    continue
            self.assertIsNotNone(server, "every declared Cortex development port is already occupied")
            assert server is not None
            port = server.server_address[1]
            server_thread = threading.Thread(target=server.serve_forever, daemon=True)
            server_thread.start()
            profile_module = REPO_ROOT / "cortex-speech-app" / "e2e_profile.cjs"
            fixture = root / "occupied-port.cjs"
            fixture.write_text(
                f"require({json.dumps(str(profile_module))})"
                f".refuseIfDebugPortBusy({port},'verifier-fault-campaign');\n",
                encoding="utf-8",
            )
            original_log_dir = self.verify.LOG_DIR
            self.verify.LOG_DIR = root
            try:
                status, _seconds, detail = self.verify.run_gate(
                    "occupied-port-campaign",
                    "cmd",
                    f'"{node}" "{fixture}"',
                    root,
                    None,
                    timeout=60,
                )
            finally:
                self.verify.LOG_DIR = original_log_dir
                server.shutdown()
                server.server_close()
                server_thread.join(timeout=5)
            self.assertEqual(status, self.verify.FAIL)
            self.assertIn(f"debug port {port} is already answering", detail)
            self.assertEqual(AnsweringDebugPort.requests, 1)
            self.assertEqual(len(list(root.glob("occupied-port-campaign.attempt-*.log"))), 1)

    def test_residual_inventory_measures_process_port_and_owned_lease(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            child = subprocess.Popen(
                [sys.executable, "-c", "import time;time.sleep(120)"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            server = None
            try:
                creation = self.supervisor.process_creation_time(child.pid)
                self.assertIsNotNone(creation)
                identities = self.verify._process_tree_identities(child.pid)
                self.assertIn(
                    {"pid": child.pid, "processCreationTime": creation}, identities
                )
                if os.name == "nt":
                    for declared_port in sorted(self.verify.VERIFIER_FAULT_DECLARED_PORTS):
                        try:
                            server = ThreadingHTTPServer(
                                ("127.0.0.1", declared_port), BaseHTTPRequestHandler
                            )
                            break
                        except OSError:
                            continue
                    self.assertIsNotNone(server)
                    assert server is not None
                    listeners = self.verify._declared_port_listeners()
                    self.assertTrue(
                        any(
                            item["port"] == server.server_address[1]
                            and item["pid"] == os.getpid()
                            for item in listeners
                        )
                    )
                else:
                    # The port inventory parses Windows `netstat -ano` PID ownership; its
                    # documented POSIX contract is the empty inventory. Pin that contract —
                    # this method's name is bound into VERIFIER_FAULT_SCENARIOS, so it must
                    # keep existing (and keep measuring) on every platform.
                    self.assertEqual(self.verify._declared_port_listeners(), [])
                owned = root / "owned.lease.json"
                contender = root / "contender.lease.json"
                self.supervisor.atomic_write_json(owned, {"runToken": "ours"})
                self.supervisor.atomic_write_json(contender, {"runToken": "theirs"})
                self.assertEqual(
                    self.verify._owned_lease_residuals((owned, contender), "ours"),
                    [owned.name],
                )
            finally:
                if server is not None:
                    server.server_close()
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)

    def test_stale_status_from_another_commit_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            pointer = Path(temporary) / "latest-proof.json"
            pointer.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": "stale-run",
                        "fullGitSha": "a" * 40,
                        "profile": self.verify.PROFILE_OWNER,
                        "manifest": "proofs/stale-run/manifest.json",
                        "manifestSha256": "b" * 64,
                        "productAttestation": "proofs/stale-run/product-attestation.json",
                        "productAttestationSha256": "c" * 64,
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(
                self.verify.EvidenceError,
                "wrong source/run identity",
            ):
                self.verify._validate_latest_proof(pointer, "d" * 40)

    def test_evidence_write_failure_is_terminal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(self.supervisor.EvidenceError):
                journal = self.supervisor.EvidenceJournal(Path(temporary), "token")
                journal.append("run_start")

    def test_latest_pointer_candidate_is_validated_before_replacing_prior_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            pointer = Path(temporary) / "latest-proof.json"
            self.supervisor.atomic_write_json(pointer, {"schema": 1, "value": "prior"})
            prior = pointer.read_bytes()

            def reject(_candidate: Path) -> None:
                raise self.supervisor.EvidenceError("injected candidate rejection")

            with self.assertRaisesRegex(self.supervisor.EvidenceError, "candidate rejection"):
                self.supervisor.publish_validated_json(
                    pointer,
                    {"schema": 1, "value": "forged"},
                    reject,
                )
            self.assertEqual(pointer.read_bytes(), prior)
            self.assertEqual(list(pointer.parent.glob("*.candidate")), [])

            def accept(candidate: Path) -> None:
                value = json.loads(candidate.read_text(encoding="utf-8"))
                if value != {"schema": 1, "value": "new"}:
                    raise self.supervisor.EvidenceError("unexpected candidate")

            self.supervisor.publish_validated_json(
                pointer,
                {"schema": 1, "value": "new"},
                accept,
            )
            self.assertEqual(json.loads(pointer.read_text(encoding="utf-8"))["value"], "new")

            prior = pointer.read_bytes()
            validations = 0

            def fail_after_publication(candidate: Path) -> None:
                nonlocal validations
                validations += 1
                value = json.loads(candidate.read_text(encoding="utf-8"))
                if value != {"schema": 1, "value": "candidate"}:
                    raise self.supervisor.EvidenceError("unexpected rollback candidate")
                if validations == 2:
                    raise self.supervisor.EvidenceError("injected public-name rejection")

            with self.assertRaisesRegex(
                self.supervisor.EvidenceError,
                "public-name rejection",
            ):
                self.supervisor.publish_validated_json(
                    pointer,
                    {"schema": 1, "value": "candidate"},
                    fail_after_publication,
                )
            self.assertEqual(pointer.read_bytes(), prior)

            absent_pointer = Path(temporary) / "first-publication.json"
            validations = 0
            with self.assertRaisesRegex(
                self.supervisor.EvidenceError,
                "public-name rejection",
            ):
                self.supervisor.publish_validated_json(
                    absent_pointer,
                    {"schema": 1, "value": "candidate"},
                    fail_after_publication,
                )
            self.assertFalse(absent_pointer.exists())

    @_requires_windows_live_authority
    def test_gate_worker_isolated_result_is_hash_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lease = self.supervisor.LeaseManager(
                root / "lease.json", "e" * 40, "owner-product", "token"
            )
            lease.acquire()
            try:
                journal = self.supervisor.EvidenceJournal(root / "events.jsonl", "token")
                gate = self.verify._gate_by_id("manifest-alignment")
                authority = self.verify._run_authority_document(diagnostic_overrides=False)
                authority_mode, authority_digest = self.verify._validate_run_authority(authority)
                (
                    status,
                    _seconds,
                    _detail,
                    artifacts,
                    environment_authority,
                    attempt_authority,
                ) = self.verify._run_gate_worker(
                    gate,
                    root / "run",
                    "token",
                    lease,
                    journal,
                    authority_mode=authority_mode,
                    run_authority_digest=authority_digest,
                )
            finally:
                lease.release()
            self.assertEqual(status, self.verify.PASS, _detail)
            self.assertTrue(artifacts)
            self.assertTrue(all(len(str(artifact["sha256"])) == 64 for artifact in artifacts))
            self.assertEqual(environment_authority["runAuthorityDigest"], authority_digest)
            self.assertEqual(
                attempt_authority,
                {"attemptCount": 1, "retryCount": 0, "retryReasons": []},
            )

    @_requires_windows_live_authority
    def test_retry_is_structured_in_worker_result_and_fsynced_journal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            run_dir = root / "run"
            lease = self.supervisor.LeaseManager(
                root / "lease.json", "e" * 40, "owner-product", "retry-token"
            )
            lease.acquire()
            journal = self.supervisor.EvidenceJournal(
                root / "events.jsonl", "retry-token"
            )
            gate = self.verify._gate_by_id("manifest-alignment")
            authority = self.verify._run_authority_document(diagnostic_overrides=False)
            authority_mode, authority_digest = self.verify._validate_run_authority(authority)
            expected_environment = self.verify._gate_environment_authority(
                gate,
                self.verify._gate_environment(gate, authority_mode),
                authority_mode=authority_mode,
                run_authority_digest=authority_digest,
            )

            class FakeProcess:
                pid = 424_244

            def complete_with_retry(_process, _job, *, timeout, heartbeat):
                self.assertEqual(timeout, gate.timeout_seconds)
                heartbeat()
                gate_dir = run_dir / "gates" / gate.id
                artifacts = []
                for attempt in (1, 2):
                    attempt_path = gate_dir / f"attempt-{attempt}.log"
                    attempt_path.write_text(f"attempt {attempt}\n", encoding="utf-8")
                    artifacts.append(
                        {
                            "path": attempt_path.name,
                            "sha256": self.supervisor.sha256_file(attempt_path),
                            "bytes": attempt_path.stat().st_size,
                        }
                    )
                self.supervisor.atomic_write_json(
                    gate_dir / "worker-result.json",
                    {
                        "schema": 1,
                        "runToken": "retry-token",
                        "gateId": gate.id,
                        "startedAt": "2026-08-28T00:00:00Z",
                        "endedAt": "2026-08-28T00:00:01Z",
                        "status": self.verify.PASS_AFTER_RETRY,
                        "seconds": 1.0,
                        "detail": "diagnostic retry",
                        "attemptCount": 2,
                        "retryCount": 1,
                        "retryReasons": ["LNK1104 linker file-lock flake"],
                        "artifacts": artifacts,
                        "environmentAuthority": expected_environment,
                    },
                )
                return 0, False

            try:
                with mock.patch.object(
                    self.verify,
                    "spawn_isolated",
                    return_value=(FakeProcess(), object()),
                ), mock.patch.object(
                    self.verify,
                    "wait_isolated",
                    side_effect=complete_with_retry,
                ):
                    (
                        status,
                        _seconds,
                        _detail,
                        _artifacts,
                        _environment_authority,
                        attempt_authority,
                    ) = self.verify._run_gate_worker(
                        gate,
                        run_dir,
                        "retry-token",
                        lease,
                        journal,
                        authority_mode=authority_mode,
                        run_authority_digest=authority_digest,
                    )
            finally:
                lease.release()

            self.assertEqual(status, self.verify.PASS_AFTER_RETRY)
            self.assertEqual(
                attempt_authority,
                {
                    "attemptCount": 2,
                    "retryCount": 1,
                    "retryReasons": ["LNK1104 linker file-lock flake"],
                },
            )
            events = [
                json.loads(line)
                for line in (root / "events.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            retry_events = [event for event in events if event["event"] == "retry"]
            self.assertEqual(len(retry_events), 1)
            self.assertEqual(retry_events[0]["attempt"], 2)
            self.assertEqual(
                retry_events[0]["reason"], "LNK1104 linker file-lock flake"
            )
            self.assertLess(
                retry_events[0]["sequence"],
                next(event["sequence"] for event in events if event["event"] == "gate_end"),
            )

    @_requires_windows_live_authority
    def test_completed_manifest_is_the_only_status_and_latest_pointer_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixed_checkout_digest = self.verify._checkout_state_digest()
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
                self.verify._assert_source_state,
                self.verify._checkout_state_digest,
            )
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = [self.verify._gate_by_id("manifest-alignment")]
                # Other deep-audit agents share this checkout and may commit unrelated files while
                # this isolated manifest test runs. Production retains the byte-drift assertion;
                # this test exercises proof semantics with a fixed synthetic gate registry.
                self.verify._assert_source_state = lambda *_args: None
                self.verify._checkout_state_digest = lambda: fixed_checkout_digest
                status_path = root / "STATUS.md"
                with mock.patch.object(
                    self.verify,
                    "_consume_rust_coverage_prerequisite",
                    side_effect=self._synthetic_coverage_binding,
                ), mock.patch.object(
                    self.verify,
                    "_release_artifact_bindings",
                    side_effect=self._stable_active_release_binding,
                ):
                    code = self.verify.aggregate_main(
                        quick=False,
                        status_md=str(status_path),
                        profile=self.verify.PROFILE_OWNER,
                    )
                self.assertEqual(code, 2, "external evidence must keep a one-gate proof incomplete")
                pointer = json.loads(self.verify.LATEST_PROOF.read_text(encoding="utf-8"))
                manifest_path = (self.verify.LATEST_PROOF.parent / pointer["manifest"]).resolve()
                self.assertEqual(pointer["manifestSha256"], self.supervisor.sha256_file(manifest_path))
                attestation_path = (
                    self.verify.LATEST_PROOF.parent / pointer["productAttestation"]
                ).resolve()
                self.assertEqual(
                    pointer["productAttestationSha256"],
                    self.supervisor.sha256_file(attestation_path),
                )
                with mock.patch.object(
                    self.verify,
                    "_release_artifact_bindings",
                    side_effect=self._stable_active_release_binding,
                ):
                    self.verify._validate_latest_proof(
                        self.verify.LATEST_PROOF, pointer["fullGitSha"]
                    )
                    with self.assertRaisesRegex(
                        self.verify.EvidenceError,
                        "not certification-eligible",
                    ):
                        self.verify._require_latest_certifying_proof(
                            self.verify.LATEST_PROOF,
                            self.verify.PROFILE_OWNER,
                            pointer["fullGitSha"],
                        )
                with mock.patch.object(
                    self.verify,
                    "_revalidate_latest_release_executable",
                    side_effect=self.verify.EvidenceError("injected active release drift"),
                ):
                    with self.assertRaisesRegex(
                        self.verify.EvidenceError,
                        "active release drift",
                    ):
                        self.verify._validate_latest_proof(
                            self.verify.LATEST_PROOF, pointer["fullGitSha"]
                        )
                changed_checkout_digest = (
                    ("0" if fixed_checkout_digest[0] != "0" else "1")
                    + fixed_checkout_digest[1:]
                )
                with mock.patch.object(
                    self.verify,
                    "_checkout_state_digest",
                    return_value=changed_checkout_digest,
                ):
                    with self.assertRaisesRegex(
                        self.verify.EvidenceError,
                        "checkout state differs from the current working tree",
                    ):
                        self.verify._validate_latest_proof(
                            self.verify.LATEST_PROOF, pointer["fullGitSha"]
                        )
                    historical = self.verify._validate_completed_manifest(
                        manifest_path, pointer["fullGitSha"], pointer["runToken"]
                    )
                    self.assertTrue(historical["complete"])
                manifest = self.verify._validate_completed_manifest(
                    manifest_path, pointer["fullGitSha"], pointer["runToken"]
                )
                self.assertTrue(manifest["complete"])
                self.assertFalse(manifest["certificationEligible"])
                self.assertEqual(
                    manifest["requiredEvidencePending"],
                    list(self.verify.PROFILE_REQUIRED_EVIDENCE[self.verify.PROFILE_OWNER]),
                )
                self.assertEqual(
                    manifest["evidenceContractHash"], self.verify.evidence_contract_hash()
                )
                attestation = json.loads(attestation_path.read_text(encoding="utf-8"))
                self.assertEqual(attestation["type"], "ProductAttestationV1")
                self.assertEqual(
                    attestation["proofManifest"]["sha256"],
                    self.supervisor.sha256_file(manifest_path),
                )
                self.assertEqual(attestation["source"]["fullGitSha"], pointer["fullGitSha"])
                self.assertEqual(attestation["releaseEnvironment"], manifest["environment"])
                self.assertEqual(attestation["schemaAuthority"], manifest["schemaAuthority"])
                self.assertIsNone(manifest["windowsReleaseAuthority"])
                self.assertIsNone(attestation["windowsReleaseAuthority"])
                self.assertFalse(status_path.exists(), "tracked/external status publication is retired")
                status = (manifest_path.parent / "STATUS.md").read_text(encoding="utf-8")
                self.assertIn(pointer["fullGitSha"], status)
                self.assertIn("owner-product", status)

                forged_status_run = (
                    self.verify.PROOF_ROOT / "forged-status" / pointer["runToken"]
                )
                forged_status_run.parent.mkdir(parents=True, exist_ok=False)
                shutil.copytree(manifest_path.parent, forged_status_run)
                forged_status_path = forged_status_run / "STATUS.md"
                forged_status_path.write_text(
                    status.replace("**Verdict:**", "**Verdict:** FALSE OWNER CLAIM —"),
                    encoding="utf-8",
                )
                forged_status_manifest_path = forged_status_run / "manifest.json"
                forged_status_manifest = json.loads(
                    forged_status_manifest_path.read_text(encoding="utf-8")
                )
                for artifact in forged_status_manifest["artifacts"]:
                    if artifact["path"] == "STATUS.md":
                        artifact["sha256"] = self.supervisor.sha256_file(
                            forged_status_path
                        )
                        artifact["bytes"] = forged_status_path.stat().st_size
                        break
                self.supervisor.atomic_write_json(
                    forged_status_manifest_path, forged_status_manifest
                )
                self.supervisor.atomic_write_json(
                    forged_status_run / self.verify.PRODUCT_ATTESTATION_NAME,
                    self.verify._product_attestation_document(
                        forged_status_manifest_path, forged_status_manifest
                    ),
                )
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "canonical manifest projection"
                ):
                    self.verify._validate_completed_manifest(
                        forged_status_manifest_path,
                        pointer["fullGitSha"],
                        pointer["runToken"],
                    )

                # Re-hashing a changed worker result and both public envelopes must not let the
                # manifest report different gate truth. The worker document is an independent
                # authority, not merely another opaque file in the global hash inventory.
                forged_worker_run = (
                    self.verify.PROOF_ROOT / "forged-worker-result" / pointer["runToken"]
                )
                forged_worker_run.parent.mkdir(parents=True, exist_ok=False)
                shutil.copytree(manifest_path.parent, forged_worker_run)
                forged_worker_result_path = (
                    forged_worker_run
                    / "gates"
                    / "manifest-alignment"
                    / "worker-result.json"
                )
                forged_worker_result = json.loads(
                    forged_worker_result_path.read_text(encoding="utf-8")
                )
                forged_worker_result["detail"] = "rehashed but contradictory worker truth"
                self.supervisor.atomic_write_json(
                    forged_worker_result_path, forged_worker_result
                )
                forged_worker_manifest_path = forged_worker_run / "manifest.json"
                forged_worker_manifest = json.loads(
                    forged_worker_manifest_path.read_text(encoding="utf-8")
                )
                worker_result_relative = str(
                    forged_worker_result_path.relative_to(forged_worker_run)
                )
                for artifact in forged_worker_manifest["artifacts"]:
                    if artifact["path"] == worker_result_relative:
                        artifact["sha256"] = self.supervisor.sha256_file(
                            forged_worker_result_path
                        )
                        artifact["bytes"] = forged_worker_result_path.stat().st_size
                        break
                else:
                    self.fail("synthetic proof omitted its worker-result binding")
                for artifact in forged_worker_manifest["results"][0]["artifacts"]:
                    if artifact["path"] == worker_result_relative:
                        artifact["sha256"] = self.supervisor.sha256_file(
                            forged_worker_result_path
                        )
                        artifact["bytes"] = forged_worker_result_path.stat().st_size
                        break
                else:
                    self.fail("synthetic gate result omitted its worker-result binding")
                self.supervisor.atomic_write_json(
                    forged_worker_manifest_path, forged_worker_manifest
                )
                self.supervisor.atomic_write_json(
                    forged_worker_run / self.verify.PRODUCT_ATTESTATION_NAME,
                    self.verify._product_attestation_document(
                        forged_worker_manifest_path, forged_worker_manifest
                    ),
                )
                with self.assertRaisesRegex(
                    self.verify.EvidenceError,
                    "differs from its independently written worker result",
                ):
                    self.verify._validate_completed_manifest(
                        forged_worker_manifest_path,
                        pointer["fullGitSha"],
                        pointer["runToken"],
                    )

                duplicate_manifest_path = root / "duplicate-manifest.json"
                duplicate_manifest_path.write_text(
                    '{"schema":1,"schema":1}\n', encoding="utf-8"
                )
                with self.assertRaisesRegex(
                    self.verify.EvidenceError, "duplicate JSON key"
                ):
                    self.verify._validate_completed_manifest(
                        duplicate_manifest_path,
                        pointer["fullGitSha"],
                        pointer["runToken"],
                    )
                with self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_completed_manifest(
                        manifest_path, "f" * 40, pointer["runToken"]
                    )

                # Re-hash both envelope files after semantic substitution. Hash chaining alone must
                # not let an omitted evidence class or alternate schema digest become authoritative.
                def assert_rehashed_substitution_rejected(label, mutate):
                    forged_run = (
                        self.verify.PROOF_ROOT / f"forged-{label}" / pointer["runToken"]
                    )
                    forged_run.parent.mkdir(parents=True, exist_ok=False)
                    shutil.copytree(manifest_path.parent, forged_run)
                    forged_manifest_path = forged_run / "manifest.json"
                    forged_manifest = json.loads(forged_manifest_path.read_text(encoding="utf-8"))
                    mutate(forged_manifest)
                    self.supervisor.atomic_write_json(forged_manifest_path, forged_manifest)
                    forged_attestation_path = forged_run / self.verify.PRODUCT_ATTESTATION_NAME
                    self.supervisor.atomic_write_json(
                        forged_attestation_path,
                        self.verify._product_attestation_document(
                            forged_manifest_path, forged_manifest
                        ),
                    )
                    forged_pointer_path = root / f"latest-{label}.json"
                    forged_pointer = {
                        **pointer,
                        "manifest": os.path.relpath(forged_manifest_path, root),
                        "manifestSha256": self.supervisor.sha256_file(forged_manifest_path),
                        "productAttestation": os.path.relpath(forged_attestation_path, root),
                        "productAttestationSha256": self.supervisor.sha256_file(
                            forged_attestation_path
                        ),
                    }
                    self.supervisor.atomic_write_json(forged_pointer_path, forged_pointer)
                    with self.assertRaises(self.verify.EvidenceError):
                        self.verify._validate_latest_proof(
                            forged_pointer_path, pointer["fullGitSha"]
                        )

                assert_rehashed_substitution_rejected(
                    "schema-substitution",
                    lambda value: value["schemaAuthority"].__setitem__(
                        "catalogSha256", "0" * 64
                    ),
                )
                assert_rehashed_substitution_rejected(
                    "evidence-omission",
                    lambda value: (
                        value.__setitem__(
                            "certificationEvidence", value["certificationEvidence"][:-1]
                        ),
                        value.__setitem__(
                            "requiredEvidencePending", value["requiredEvidencePending"][:-1]
                        ),
                    ),
                )

                forged_profile_pointer = root / "latest-wrong-profile.json"
                self.supervisor.atomic_write_json(
                    forged_profile_pointer,
                    {**pointer, "profile": self.verify.PROFILE_WINDOWS},
                )
                with self.assertRaisesRegex(self.verify.EvidenceError, "profile differs"):
                    self.verify._validate_latest_proof(
                        forged_profile_pointer, pointer["fullGitSha"]
                    )

                original_attestation = attestation_path.read_bytes()
                try:
                    forged_attestation = json.loads(original_attestation.decode("utf-8"))
                    forged_attestation["proofManifest"]["sha256"] = "0" * 64
                    self.supervisor.atomic_write_json(attestation_path, forged_attestation)
                    with self.assertRaisesRegex(self.verify.EvidenceError, "attestation hash"):
                        self.verify._validate_latest_proof(
                            self.verify.LATEST_PROOF, pointer["fullGitSha"]
                        )
                finally:
                    self.supervisor.atomic_write_bytes(attestation_path, original_attestation)

                original_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                corruptions = {
                    "missing-result": {**original_manifest, "results": []},
                    "forged-green": {
                        **original_manifest,
                        "exitCode": 0,
                        "verdict": "CORTEX PRODUCT 10/10 — OWNER WORKSTATION",
                    },
                    "wrong-registry": {**original_manifest, "gateRegistryHash": "0" * 64},
                    "wrong-tree": {**original_manifest, "sourceTreeDigest": "0" * 40},
                    "traversal": {
                        **original_manifest,
                        "artifacts": [
                            {**original_manifest["artifacts"][0], "path": "../outside"},
                            *original_manifest["artifacts"][1:],
                        ],
                    },
                }
                for label, forged in corruptions.items():
                    forged_path = manifest_path.parent / f"forged-{label}.json"
                    forged_path.write_text(json.dumps(forged), encoding="utf-8")
                    with self.subTest(label=label), self.assertRaises(self.verify.EvidenceError):
                        self.verify._validate_completed_manifest(
                            forged_path, pointer["fullGitSha"], pointer["runToken"]
                        )
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                    self.verify._assert_source_state,
                    self.verify._checkout_state_digest,
                ) = original

    def test_stale_takeover_can_recover_but_can_never_certify(self) -> None:
        verified_evidence = [
            {
                "classId": spec.id,
                "status": "VERIFIED",
                "detail": "class-specific validator output",
            }
            for spec in self.verify._required_evidence_specs(self.verify.PROFILE_OWNER)
        ]
        passing_results = [("synthetic", self.verify.PASS, 0.1, "")]
        clean_code, clean_verdict = self.verify._profile_verdict(
            self.verify.PROFILE_OWNER,
            False,
            passing_results,
            verified_evidence,
        )
        recovered_code, recovered_verdict = self.verify._profile_verdict(
            self.verify.PROFILE_OWNER,
            False,
            passing_results,
            verified_evidence,
            stale_takeover=True,
        )
        self.assertEqual(clean_code, 0)
        self.assertEqual(clean_verdict, "CORTEX PRODUCT 10/10 — OWNER WORKSTATION")
        self.assertEqual(recovered_code, 2)
        self.assertIn("stale-lock takeover occurred", recovered_verdict)
        self.assertIn("fresh no-takeover run", recovered_verdict)

    @_requires_windows_live_authority
    def test_takeover_is_bound_into_manifest_attestation_and_terminal_journal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixed_checkout_digest = self.verify._checkout_state_digest()
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
                self.verify._assert_source_state,
                self.verify._checkout_state_digest,
            )
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = []
                self.verify._assert_source_state = lambda *_args: None
                self.verify._checkout_state_digest = lambda: fixed_checkout_digest
                with mock.patch.object(
                    self.verify.LeaseManager,
                    "acquire",
                    return_value="abandoned-run-token",
                ), mock.patch.object(
                    self.verify,
                    "_consume_rust_coverage_prerequisite",
                    side_effect=self._synthetic_coverage_binding,
                ), mock.patch.object(
                    self.verify,
                    "_release_artifact_bindings",
                    side_effect=self._stable_active_release_binding,
                ):
                    code = self.verify.aggregate_main(
                        quick=False,
                        status_md=None,
                        profile=self.verify.PROFILE_OWNER,
                    )
                self.assertEqual(code, 2)
                pointer = json.loads(self.verify.LATEST_PROOF.read_text(encoding="utf-8"))
                manifest_path = (self.verify.LATEST_PROOF.parent / pointer["manifest"]).resolve()
                manifest = self.verify._validate_completed_manifest(
                    manifest_path,
                    pointer["fullGitSha"],
                    pointer["runToken"],
                )
                self.assertEqual(
                    manifest["staleTakeover"],
                    {"occurred": True, "abandonedRunToken": "abandoned-run-token"},
                )
                self.assertFalse(manifest["certificationEligible"])
                self.assertIn("fresh no-takeover run", manifest["verdict"])
                attestation = json.loads(
                    (manifest_path.parent / self.verify.PRODUCT_ATTESTATION_NAME).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(attestation["staleTakeover"], manifest["staleTakeover"])
                events = [
                    json.loads(line)
                    for line in (manifest_path.parent / "events.jsonl")
                    .read_text(encoding="utf-8")
                    .splitlines()
                ]
                abandonments = [event for event in events if event["event"] == "abandonment"]
                self.assertEqual(len(abandonments), 1)
                self.assertEqual(abandonments[0]["abandonedRunToken"], "abandoned-run-token")
                self.assertIs(events[-1]["staleTakeover"], True)

                forged_run = root / "forged-takeover-removal"
                shutil.copytree(manifest_path.parent, forged_run)
                forged_manifest_path = forged_run / "manifest.json"
                forged_manifest = json.loads(forged_manifest_path.read_text(encoding="utf-8"))
                forged_manifest["staleTakeover"] = {
                    "occurred": False,
                    "abandonedRunToken": None,
                }
                _, forged_verdict = self.verify._profile_verdict(
                    self.verify.PROFILE_OWNER,
                    False,
                    [],
                    forged_manifest["certificationEvidence"],
                )
                forged_manifest["verdict"] = forged_verdict
                forged_status_path = forged_run / "STATUS.md"
                self.supervisor.atomic_write_bytes(
                    forged_status_path,
                    self.verify._status_md_text(
                        forged_manifest["fullGitSha"],
                        forged_manifest["quick"],
                        [
                            (
                                result["gateId"],
                                result["status"],
                                result["seconds"],
                                result["detail"],
                            )
                            for result in forged_manifest["results"]
                        ],
                        forged_manifest["verdict"],
                        forged_manifest["profile"],
                        forged_manifest["certificationEvidence"],
                    ).encode("utf-8"),
                )
                for artifact in forged_manifest["artifacts"]:
                    if artifact["path"] == "STATUS.md":
                        artifact["sha256"] = self.supervisor.sha256_file(
                            forged_status_path
                        )
                        artifact["bytes"] = forged_status_path.stat().st_size
                        break
                self.supervisor.atomic_write_json(forged_manifest_path, forged_manifest)
                self.supervisor.atomic_write_json(
                    forged_run / self.verify.PRODUCT_ATTESTATION_NAME,
                    self.verify._product_attestation_document(
                        forged_manifest_path,
                        forged_manifest,
                    ),
                )
                with self.assertRaisesRegex(
                    self.verify.EvidenceError,
                    "terminal run_end|takeover manifest and journal",
                ):
                    self.verify._validate_completed_manifest(
                        forged_manifest_path,
                        pointer["fullGitSha"],
                        pointer["runToken"],
                    )
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                    self.verify._assert_source_state,
                    self.verify._checkout_state_digest,
                ) = original

    @_requires_windows_live_authority
    def test_attestation_publication_failure_invalidates_the_run_and_publishes_no_pointer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
                self.verify._assert_source_state,
                self.verify.atomic_write_json,
            )
            real_atomic_write_json = self.verify.atomic_write_json
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = [self.verify._gate_by_id("manifest-alignment")]
                self.verify._assert_source_state = lambda *_args: None

                def fail_attestation(path, value):
                    if Path(path).name == self.verify.PRODUCT_ATTESTATION_NAME:
                        raise self.verify.EvidenceError("injected attestation write failure")
                    return real_atomic_write_json(path, value)

                self.verify.atomic_write_json = fail_attestation
                with mock.patch.object(
                    self.verify,
                    "_consume_rust_coverage_prerequisite",
                    side_effect=self._synthetic_coverage_binding,
                ):
                    code = self.verify.aggregate_main(
                        quick=False,
                        status_md=None,
                        profile=self.verify.PROFILE_OWNER,
                    )
                self.assertEqual(code, 1)
                self.assertFalse(self.verify.LATEST_PROOF.exists())
                runs = list(self.verify.PROOF_ROOT.iterdir())
                self.assertEqual(len(runs), 1)
                events = [
                    json.loads(line)
                    for line in (runs[0] / "events.jsonl").read_text(encoding="utf-8").splitlines()
                ]
                self.assertEqual(events[-1]["event"], "publication_failure")
                self.assertEqual(events[-1]["verdict"], "VERIFIER FAILURE")
                self.assertIn("injected attestation write failure", events[-1]["detail"])
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                    self.verify._assert_source_state,
                    self.verify.atomic_write_json,
                ) = original


if __name__ == "__main__":
    unittest.main()
