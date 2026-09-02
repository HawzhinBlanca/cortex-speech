"""Adversarial regressions for exact Windows artifact/provenance binding."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
MODULE = REPO_ROOT / "scripts" / "windows_release_bundle.py"


def load_module():
    spec = importlib.util.spec_from_file_location("windows_release_bundle_test", MODULE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleaseBundleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.bundle = load_module()
        cls.sha = "a" * 40
        cls.repo = "owner/repository"
        cls.ref = "refs/tags/v2.1.0"
        cls.thumbprint = "B" * 40
        cls.cert_sha256 = "C" * 64

    def _write_bundle(self, root: Path, *, updater: bool = False) -> None:
        (root / self.bundle.APPLICATION_NAME).write_bytes(
            b"MZ\x00CORTEX_BUILD_SHA:" + self.sha.encode("ascii") + b"\x00app"
        )
        (root / "Cortex_2.1.0_x64_en-US.msi").write_bytes(b"msi")
        (root / "Cortex_2.1.0_x64-setup.exe").write_bytes(b"MZnsis")
        (root / self.bundle.SBOM_NAME).write_text(
            json.dumps(
                {
                    "bomFormat": "CycloneDX",
                    "specVersion": "1.5",
                    "version": 1,
                    "metadata": {
                        "component": {
                            "type": "application",
                            "name": "cortex-speech-app",
                            "version": "2.1.0",
                        }
                    },
                    "components": [
                        {
                            "type": "library",
                            "name": "a",
                            "version": "1",
                            "purl": "pkg:cargo/a@1",
                        }
                    ],
                },
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        (root / self.bundle.ENVIRONMENT_NAME).write_text(
            json.dumps(
                {
                    "schema": 1,
                    "source": {
                        "gitSha": self.sha,
                        "repository": self.repo,
                        "ref": self.ref,
                        "workflow": f"{self.repo}/.github/workflows/release.yml",
                    },
                    "runner": {"os": "Windows", "arch": "X64"},
                    "tools": {
                        "rustc": "rustc test",
                        "cargo": "cargo test",
                        "node": "v22",
                        "npm": "10",
                        "python": "Python 3.12",
                        "tauriCli": "tauri-cli 2",
                    },
                    "inputs": {},
                }
            ),
            encoding="utf-8",
        )
        (root / self.bundle.PROVENANCE_NAME).write_text("{}", encoding="utf-8")
        if updater:
            (root / "Cortex_2.1.0_x64.nsis.zip").write_bytes(b"updater")
            (root / "Cortex_2.1.0_x64.nsis.zip.sig").write_bytes(b"signature")
        self._rewrite_checksums(root)

    def _rewrite_checksums(self, root: Path) -> None:
        manifest = root / self.bundle.CHECKSUM_NAME
        files = sorted(
            (path for path in root.iterdir() if path.is_file() and path != manifest),
            key=lambda path: path.name,
        )
        manifest.write_text(
            "".join(
                f"{hashlib.sha256(path.read_bytes()).hexdigest()} *{path.name}\n"
                for path in files
            ),
            encoding="ascii",
            newline="\n",
        )

    def _validate(self, root: Path, **overrides):
        values = {
            "expected_sha": self.sha,
            "expected_repository": self.repo,
            "expected_ref": self.ref,
            "expected_version": "2.1.0",
            "signer_thumbprint": self.thumbprint,
            "signer_cert_sha256": self.cert_sha256,
            "repo_root": None,
            "verify_authenticode": False,
            "verify_provenance": False,
            "require_windows_product": False,
        }
        values.update(overrides)
        return self.bundle.validate_bundle(root, **values)

    def test_draft_authority_is_exact_but_cannot_impersonate_windows_product(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_bundle(root)
            authority = self._validate(root)
            self.assertFalse(authority["certificationReady"])
            roles = {artifact["role"] for artifact in authority["artifacts"]}
            self.assertEqual(
                roles,
                {
                    "application-executable",
                    "windows-msi",
                    "windows-nsis",
                    "release-checksums",
                    "cyclonedx-sbom",
                    "github-sigstore-provenance",
                    "release-environment",
                },
            )
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "updater"):
                self._validate(root, require_windows_product=True)

    def test_certifying_authority_requires_real_crypto_callbacks_and_updater_pair(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_bundle(root, updater=True)
            auth = {
                "status": "Valid",
                "signerThumbprintSha1": self.thumbprint,
                "signerCertificateSha256": self.cert_sha256,
                "timestampVerified": True,
                "timestampAuthoritySubject": "CN=Timestamp Authority",
            }
            with (
                mock.patch.object(self.bundle, "_verify_authenticode", return_value=auth) as signed,
                mock.patch.object(self.bundle, "_verify_provenance") as provenance,
                mock.patch.object(self.bundle, "_validate_environment"),
                mock.patch.object(self.bundle, "_validate_sbom"),
                mock.patch.object(
                    self.bundle,
                    "_verify_updater_signature",
                    return_value={
                        "verified": True,
                        "publicKeySha256": "d" * 64,
                        "verifierSourceSha256": "e" * 64,
                        "verifier": "minisign-verify-0.2.5/tauri-v2-strict",
                    },
                ) as updater_verified,
            ):
                authority = self._validate(
                    root,
                    repo_root=REPO_ROOT,
                    verify_authenticode=True,
                    verify_provenance=True,
                    require_windows_product=True,
                )
            self.assertTrue(authority["certificationReady"])
            self.assertEqual(signed.call_count, 3)
            provenance.assert_called_once()
            updater = next(
                artifact for artifact in authority["artifacts"] if artifact["role"] == "windows-updater"
            )
            self.assertEqual(updater["signature"]["name"], "Cortex_2.1.0_x64.nsis.zip.sig")
            self.assertTrue(updater["signature"]["verified"])
            updater_verified.assert_called_once()

    def test_updater_signature_uses_pinned_runtime_equivalent_verifier_and_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "update.nsis.zip"
            signature = root / "update.nsis.zip.sig"
            archive.write_bytes(b"archive")
            signature.write_text("signature", encoding="ascii")
            success = subprocess.CompletedProcess([], 0, stdout="VERIFIED", stderr="")
            with (
                mock.patch.object(self.bundle, "_configured_updater_public_key", return_value="public-key"),
                mock.patch.object(self.bundle, "_updater_verifier_source_identity", return_value="f" * 64),
                mock.patch.object(self.bundle.shutil, "which", return_value="cargo.exe"),
                mock.patch.object(self.bundle.subprocess, "run", return_value=success) as run,
            ):
                result = self.bundle._verify_updater_signature(
                    archive, signature, repo_root=REPO_ROOT
                )
            argv = run.call_args.args[0]
            self.assertEqual(argv[:4], ["cargo.exe", "run", "--quiet", "--locked"])
            self.assertIn("--release", argv)
            self.assertIn("--manifest-path", argv)
            self.assertEqual(argv[-3:], [str(archive), str(signature), "public-key"])
            self.assertIs(run.call_args.kwargs["shell"], False)
            self.assertTrue(result["verified"])

            failure = subprocess.CompletedProcess([], 1, stdout="", stderr="wrong key")
            with (
                mock.patch.object(self.bundle, "_configured_updater_public_key", return_value="public-key"),
                mock.patch.object(self.bundle.shutil, "which", return_value="cargo.exe"),
                mock.patch.object(self.bundle.subprocess, "run", return_value=failure),
                self.assertRaisesRegex(self.bundle.ReleaseBundleError, "signature verification failed"),
            ):
                self.bundle._verify_updater_signature(archive, signature, repo_root=REPO_ROOT)

    def test_hash_substitution_undeclared_file_and_wrong_build_sha_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_bundle(root)
            (root / "Cortex_2.1.0_x64_en-US.msi").write_bytes(b"tampered")
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "checksum mismatch"):
                self._validate(root)

            self._write_bundle(root)
            (root / "undeclared.txt").write_text("surprise", encoding="utf-8")
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "checksum inventory is not exact"):
                self._validate(root)

            (root / "undeclared.txt").unlink()
            (root / self.bundle.APPLICATION_NAME).write_bytes(
                b"MZ\x00CORTEX_BUILD_SHA:" + ("d" * 40).encode("ascii")
            )
            self._rewrite_checksums(root)
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "carries build SHA"):
                self._validate(root)

    def test_checksum_traversal_and_case_collision_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_bundle(root)
            manifest = root / self.bundle.CHECKSUM_NAME
            manifest.write_text("0" * 64 + " *../outside\n", encoding="ascii")
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "unsafe path"):
                self._validate(root)

            self._write_bundle(root)
            manifest = root / self.bundle.CHECKSUM_NAME
            manifest.write_text(
                manifest.read_text(encoding="ascii")
                + "0" * 64
                + " *CORTEX-SPEECH-APP.EXE\n",
                encoding="ascii",
            )
            with self.assertRaisesRegex(self.bundle.ReleaseBundleError, "case-colliding"):
                self._validate(root)

    def test_sigstore_verifier_pins_bundle_workflow_source_and_hosted_runner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subject = root / "subject.exe"
            bundle = root / "bundle.json"
            subject.write_bytes(b"subject")
            bundle.write_text("{}", encoding="utf-8")
            success = subprocess.CompletedProcess([], 0, stdout="verified", stderr="")
            with (
                mock.patch.object(self.bundle.shutil, "which", return_value="gh.exe"),
                mock.patch.object(self.bundle.subprocess, "run", return_value=success) as run,
            ):
                self.bundle._verify_provenance(
                    [subject],
                    bundle,
                    repository=self.repo,
                    source_sha=self.sha,
                    source_ref=self.ref,
                )
            argv = run.call_args.args[0]
            self.assertIn("--bundle", argv)
            self.assertIn(str(bundle), argv)
            self.assertIn("--signer-workflow", argv)
            self.assertIn(f"{self.repo}/.github/workflows/release.yml", argv)
            self.assertIn("--source-digest", argv)
            self.assertIn(self.sha, argv)
            self.assertIn("--source-ref", argv)
            self.assertIn(self.ref, argv)
            self.assertIn("--deny-self-hosted-runners", argv)
            self.assertIs(run.call_args.kwargs["shell"], False)

            failure = subprocess.CompletedProcess([], 1, stdout="", stderr="forged")
            with (
                mock.patch.object(self.bundle.shutil, "which", return_value="gh.exe"),
                mock.patch.object(self.bundle.subprocess, "run", return_value=failure),
                self.assertRaisesRegex(self.bundle.ReleaseBundleError, "provenance verification failed"),
            ):
                self.bundle._verify_provenance(
                    [subject],
                    bundle,
                    repository=self.repo,
                    source_sha=self.sha,
                    source_ref=self.ref,
                )

    def test_authenticode_verifier_pins_certificate_and_requires_timestamp(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            subject = Path(temporary) / "subject.exe"
            subject.write_bytes(b"MZ")
            signtool_success = subprocess.CompletedProcess([], 0, stdout="verified", stderr="")
            identity = {
                "status": "Valid",
                "thumbprint": self.thumbprint,
                "certificateSha256": self.cert_sha256,
                "timestampSubject": "CN=Timestamp Authority",
            }
            powershell_success = subprocess.CompletedProcess(
                [], 0, stdout=json.dumps(identity), stderr=""
            )
            with (
                mock.patch.object(self.bundle.os, "name", "nt"),
                mock.patch.object(self.bundle, "_signtool", return_value=Path("signtool.exe")),
                mock.patch.object(self.bundle, "_powershell_executable", return_value="pwsh.exe"),
                mock.patch.object(
                    self.bundle.subprocess,
                    "run",
                    side_effect=[signtool_success, powershell_success],
                ) as run,
            ):
                authority = self.bundle._verify_authenticode(
                    subject, self.thumbprint, self.cert_sha256
                )
            self.assertTrue(authority["timestampVerified"])
            self.assertEqual(authority["signerCertificateSha256"], self.cert_sha256)
            self.assertEqual(run.call_args_list[0].args[0][1:6], ["verify", "/pa", "/all", "/v", "/tw"])

            identity["timestampSubject"] = None
            with (
                mock.patch.object(self.bundle.os, "name", "nt"),
                mock.patch.object(self.bundle, "_signtool", return_value=Path("signtool.exe")),
                mock.patch.object(self.bundle, "_powershell_executable", return_value="pwsh.exe"),
                mock.patch.object(
                    self.bundle.subprocess,
                    "run",
                    side_effect=[
                        signtool_success,
                        subprocess.CompletedProcess([], 0, stdout=json.dumps(identity), stderr=""),
                    ],
                ),
                self.assertRaisesRegex(self.bundle.ReleaseBundleError, "missing timestamp"),
            ):
                self.bundle._verify_authenticode(subject, self.thumbprint, self.cert_sha256)


def main() -> None:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(WindowsReleaseBundleTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if not result.wasSuccessful():
        raise SystemExit(1)


if __name__ == "__main__":
    main()
