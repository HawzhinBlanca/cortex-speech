"""The CI coverage attestation must fail closed on every forgeable axis.

Owner decision 2026-08-31: hosted CI verifies the workstation's hash-bound coverage manifest
instead of re-measuring (4-core runners cannot fit the instrumented phase under the workflow
policy's 180-minute cap). That verifier is now part of the merge chain, so this gate proves the
refusal matrix with a real scratch git repository: a valid attestation verifies, and every
tampered axis — envelope, ancestry, non-attestation diffs, tree digest, staleness, floor
arithmetic — is refused. The publisher's private-path hygiene refusal is pinned too.
"""

from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
VERIFY_10 = REPO_ROOT / "scripts" / "verify_10.py"


def _load_verify_10():
    spec = importlib.util.spec_from_file_location("cortex_verify10_attestation_gate", VERIFY_10)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


V10 = _load_verify_10()

# Snapshot the real registry before any test patches REPO_ROOT: it derives repo-relative paths,
# and the scratch-repo patch below would otherwise break its path arithmetic mid-verify.
RAW_REGISTRY = V10._rust_coverage_command_registry()


def _git(cwd: Path, *argv: str) -> str:
    completed = subprocess.run(
        ["git", *argv], cwd=cwd, capture_output=True, text=True, check=True
    )
    return completed.stdout.strip()


class CoverageAttestationTests(unittest.TestCase):
    def setUp(self) -> None:
        # Resolved, per the darwin lesson: macOS tempdirs are symlink aliases.
        self.tmp = Path(tempfile.mkdtemp(prefix="cortex-attestation-")).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        _git(self.repo, "init", "--quiet", "-b", "main")
        _git(self.repo, "config", "user.email", "gate@example.invalid")
        _git(self.repo, "config", "user.name", "Attestation Gate")
        (self.repo / "code.txt").write_text("v1\n", encoding="utf-8")
        _git(self.repo, "add", "code.txt")
        _git(self.repo, "commit", "--quiet", "-m", "measured commit")
        self.measured_sha = _git(self.repo, "rev-parse", "HEAD")
        self.attestation_dir = self.repo / V10.COVERAGE_ATTESTATION_DIR.name
        self.attestation_path = self.attestation_dir / V10.COVERAGE_ATTESTATION_PATH.name
        self._original = (
            V10.REPO_ROOT,
            V10.COVERAGE_ATTESTATION_DIR,
            V10.COVERAGE_ATTESTATION_PATH,
            V10._rust_coverage_command_registry,
        )
        V10.REPO_ROOT = self.repo
        V10.COVERAGE_ATTESTATION_DIR = self.attestation_dir
        V10.COVERAGE_ATTESTATION_PATH = self.attestation_path
        V10._rust_coverage_command_registry = lambda: json.loads(json.dumps(RAW_REGISTRY))
        self.addCleanup(self._restore)

    def _restore(self) -> None:
        (
            V10.REPO_ROOT,
            V10.COVERAGE_ATTESTATION_DIR,
            V10.COVERAGE_ATTESTATION_PATH,
            V10._rust_coverage_command_registry,
        ) = self._original

    def _passing_document(self) -> dict:
        registry = V10._normalized_attestation_registry(V10._rust_coverage_command_registry())
        thresholds = registry["thresholds"]
        metrics = {
            name: {"required_percent": required, "count": 1000, "covered": 1000}
            for name, required in thresholds.items()
        }
        domain_thresholds = registry["criticalDomainThresholds"]
        domains = {
            "review": {
                name: {"required_percent": required, "count": 100, "covered": 100}
                for name, required in domain_thresholds.items()
            }
        }
        ended = datetime.now(timezone.utc) - timedelta(minutes=30)
        started = ended - timedelta(minutes=50)
        expires = ended + timedelta(seconds=V10.RUST_COVERAGE_FRESH_SECONDS)
        iso = lambda value: value.strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
        manifest = {
            "schema": 1,
            "type": "RustCoveragePrerequisiteV1",
            "complete": True,
            "runToken": "a" * 32,
            "fullGitSha": self.measured_sha,
            "sourceTreeDigest": V10._source_tree_digest_for_sha(self.measured_sha),
            "checkoutStateDigest": "b" * 64,
            "startedAt": iso(started),
            "endedAt": iso(ended),
            "expiresAt": iso(expires),
            "exitCode": 0,
            "attemptCount": 1,
            "commandRegistry": registry,
            "environment": {
                "schema": 1,
                "coverageToolchain": V10._expected_rust_coverage_toolchain_identity(),
            },
            "coverage": {
                "passed": True,
                "metrics": metrics,
                "criticalDomains": domains,
                "artifactSha256": "c" * 64,
            },
            "artifacts": [],
        }
        return {
            "schema": 1,
            "type": V10.COVERAGE_ATTESTATION_TYPE,
            "publishedAt": iso(datetime.now(timezone.utc)),
            "manifest": manifest,
        }

    def _write(self, document: dict) -> None:
        self.attestation_dir.mkdir(parents=True, exist_ok=True)
        self.attestation_path.write_text(
            json.dumps(document, sort_keys=True, indent=1) + "\n", encoding="utf-8"
        )

    def _commit_attestation(self) -> None:
        _git(self.repo, "add", str(self.attestation_path.relative_to(self.repo)))
        _git(self.repo, "commit", "--quiet", "-m", "attestation only")

    def test_a_valid_attestation_verifies_at_the_measured_sha(self) -> None:
        self._write(self._passing_document())
        self.assertEqual(V10.verify_coverage_attestation_main(), 0)

    def test_an_attestation_only_commit_on_top_still_verifies(self) -> None:
        self._write(self._passing_document())
        self._commit_attestation()
        self.assertEqual(V10.verify_coverage_attestation_main(), 0)

    def test_missing_attestation_is_refused(self) -> None:
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_wrong_type_is_refused(self) -> None:
        document = self._passing_document()
        document["type"] = "SomethingElseV1"
        self._write(document)
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_a_non_ancestor_measurement_sha_is_refused(self) -> None:
        document = self._passing_document()
        document["manifest"]["fullGitSha"] = "d" * 40
        self._write(document)
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_code_changes_after_the_measured_sha_are_refused(self) -> None:
        self._write(self._passing_document())
        (self.repo / "code.txt").write_text("v2 sneaks past the measurement\n", encoding="utf-8")
        _git(self.repo, "add", "code.txt", str(self.attestation_path.relative_to(self.repo)))
        _git(self.repo, "commit", "--quiet", "-m", "code and attestation together")
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_a_tampered_tree_digest_is_refused(self) -> None:
        document = self._passing_document()
        document["manifest"]["sourceTreeDigest"] = "e" * 40
        self._write(document)
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_a_stale_attestation_is_refused(self) -> None:
        document = self._passing_document()
        manifest = document["manifest"]
        ended = datetime.now(timezone.utc) - timedelta(seconds=V10.RUST_COVERAGE_FRESH_SECONDS + 3600)
        started = ended - timedelta(minutes=50)
        expires = ended + timedelta(seconds=V10.RUST_COVERAGE_FRESH_SECONDS)
        iso = lambda value: value.strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
        manifest["startedAt"], manifest["endedAt"], manifest["expiresAt"] = (
            iso(started),
            iso(ended),
            iso(expires),
        )
        self._write(document)
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_failing_floor_arithmetic_is_refused_even_when_passed_claims_true(self) -> None:
        document = self._passing_document()
        metrics = document["manifest"]["coverage"]["metrics"]
        first = next(iter(metrics))
        metrics[first]["covered"] = 0
        self._write(document)
        self.assertEqual(V10.verify_coverage_attestation_main(), 1)

    def test_registry_normalization_and_hygiene_refuse_private_paths(self) -> None:
        registry = V10._normalized_attestation_registry(V10._rust_coverage_command_registry())
        self.assertEqual(registry["argvTemplate"][0], "<python>")
        with self.assertRaises(V10.EvidenceError):
            V10._assert_attestation_hygiene(r'{"x": "C:\\Users\\someone\\secret"}')
        with self.assertRaises(V10.EvidenceError):
            V10._assert_attestation_hygiene('{"x": "/home/someone/secret"}')


if __name__ == "__main__":
    result = unittest.main(exit=False).result
    if not result.wasSuccessful():
        sys.exit(1)
    print(f"PASS: coverage attestation policy ({result.testsRun} tests)")
    sys.exit(0)
