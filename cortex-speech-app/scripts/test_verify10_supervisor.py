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


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class Verify10SupervisorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.supervisor = load_module("verify10_supervisor_fault_test", SUPERVISOR)
        cls.verify = load_module("verify10_fault_test", VERIFY)

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
        for exact_threshold in (
            "statements: 85",
            "branches: 80",
            "functions: 80",
            "lines: 85",
        ):
            self.assertIn(exact_threshold, coverage_config)
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

    def test_rust_coverage_prerequisite_rejects_missing_wrong_stale_subthreshold_and_forged_evidence(self) -> None:
        sha = self.verify._full_git_sha()
        checkout_digest = self.verify._checkout_state_digest()

        with tempfile.TemporaryDirectory() as temporary:
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
            self.assertFalse(
                set(registry_by_id[gate_id]["environmentAllowlist"])
                & set(self.verify.LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
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
        self.assertIsNone(classes["owner-field-sessions"]["validatorGate"])

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

    def test_fault_campaign_evidence_rejects_self_authored_retry_stale_and_incomplete_reports(self) -> None:
        sha = self.verify._full_git_sha()
        registry_hash = self.verify.gate_registry_hash()
        checkout_digest = self.verify._checkout_state_digest()
        environment = self.verify._environment_document()
        now = datetime.now(timezone.utc).replace(microsecond=0)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
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
                with self.assertRaisesRegex(self.verify.EvidenceError, "incomplete"):
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

    def test_probe_crash_fails_only_its_gate(self) -> None:
        def crash():
            raise RuntimeError("injected probe crash")

        status, _seconds, detail = self.verify.run_gate(
            "probe-crash-campaign", "fn", lambda: True, None, crash
        )
        self.assertEqual(status, self.verify.FAIL)
        self.assertIn("probe crashed", detail)

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
                status, _seconds, detail = self.verify.run_gate(
                    "abnormal-node-campaign",
                    "cmd",
                    f'"{node}" "{script_path}"',
                    root,
                    None,
                    timeout=60,
                )
                killer.join(timeout=10)
                self.assertFalse(killer.is_alive())
                self.assertEqual(killer_errors, [])
                self.assertEqual(len(killed_identity), 1)
                self.assertEqual(status, self.verify.PASS_AFTER_RETRY)
                self.assertIn("re-ran once", detail)
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
                status, _seconds, _detail, artifacts, environment_authority = (
                    self.verify._run_gate_worker(
                        gate,
                        root / "run",
                        "token",
                        lease,
                        journal,
                        authority_mode=authority_mode,
                        run_authority_digest=authority_digest,
                    )
                )
            finally:
                lease.release()
            self.assertEqual(status, self.verify.PASS, _detail)
            self.assertTrue(artifacts)
            self.assertTrue(all(len(str(artifact["sha256"])) == 64 for artifact in artifacts))
            self.assertEqual(environment_authority["runAuthorityDigest"], authority_digest)

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
