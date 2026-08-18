#!/usr/bin/env python3
"""Direct stdlib tests for the one-time legacy champion manifest generator."""

from __future__ import annotations

import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SCRIPT_DIR.parent
MEASUREMENT_EVIDENCE = PROJECT_DIR / "docs" / "MEASUREMENTS.md"
EXPECTED_MEASUREMENT_EVIDENCE_SHA256 = "6133abb5684f1f9856c99b4765cfea66e084ba4acd72ece649e316c467fee68e"
EXPECTED_ADAPTER_CONFIG_SHA256 = "4b870f13ec88f4ca19cc3bdded779ec03090077a82e0381412fb80cc420ce331"


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


bootstrap = _load("bootstrap_legacy_champion_under_test", SCRIPT_DIR / "bootstrap_legacy_champion.py")

# Exact bytes of the measured live incumbent config.  Keeping this tiny fixture in the test makes
# the admission identity reproducible without copying the private 30-GB checkpoint or 76-MiB LoRA.
_LIVE_CONFIG_B64 = (
    "ewogICJhbG9yYV9pbnZvY2F0aW9uX3Rva2VucyI6IG51bGwsCiAgImFscGhhX3BhdHRlcm4iOiB7fSwKICAi"
    "YXJyb3dfY29uZmlnIjogbnVsbCwKICAiYXV0b19tYXBwaW5nIjogewogICAgImJhc2VfbW9kZWxfY2xhc3MiOiAi"
    "V2F2MlZlYzJMbGFtYU1vZGVsIiwKICAgICJwYXJlbnRfbGlicmFyeSI6ICJvbW5pbGluZ3VhbF9hc3IubW9kZWxz"
    "LndhdjJ2ZWMyX2xsYW1hLm1vZGVsIgogIH0sCiAgImJhc2VfbW9kZWxfbmFtZV9vcl9wYXRoIjogbnVsbCwKICAi"
    "YmlhcyI6ICJub25lIiwKICAiY29yZGFfY29uZmlnIjogbnVsbCwKICAiZW5zdXJlX3dlaWdodF90eWluZyI6IGZh"
    "bHNlLAogICJldmFfY29uZmlnIjogbnVsbCwKICAiZXhjbHVkZV9tb2R1bGVzIjogbnVsbCwKICAiZmFuX2luX2Zh"
    "bl9vdXQiOiBmYWxzZSwKICAiaW5mZXJlbmNlX21vZGUiOiB0cnVlLAogICJpbml0X2xvcmFfd2VpZ2h0cyI6IHRy"
    "dWUsCiAgImxheWVyX3JlcGxpY2F0aW9uIjogbnVsbCwKICAibGF5ZXJzX3BhdHRlcm4iOiBudWxsLAogICJsYXll"
    "cnNfdG9fdHJhbnNmb3JtIjogbnVsbCwKICAibG9mdHFfY29uZmlnIjoge30sCiAgImxvcmFfYWxwaGEiOiAzMiwK"
    "ICAibG9yYV9iaWFzIjogZmFsc2UsCiAgImxvcmFfZHJvcG91dCI6IDAuMDUsCiAgImxvcmFfZ2FfY29uZmlnIjog"
    "bnVsbCwKICAibWVnYXRyb25fY29uZmlnIjogbnVsbCwKICAibWVnYXRyb25fY29yZSI6ICJtZWdhdHJvbi5jb3Jl"
    "IiwKICAibW9kdWxlc190b19zYXZlIjogbnVsbCwKICAicGVmdF90eXBlIjogIkxPUkEiLAogICJwZWZ0X3ZlcnNp"
    "b24iOiAiMC4xOS4xIiwKICAicWFsb3JhX2dyb3VwX3NpemUiOiAxNiwKICAiciI6IDE2LAogICJyYW5rX3BhdHRl"
    "cm4iOiB7fSwKICAicmV2aXNpb24iOiBudWxsLAogICJ0YXJnZXRfbW9kdWxlcyI6IFsKICAgICJxX3Byb2oiLAog"
    "ICAgInZfcHJvaiIKICBdLAogICJ0YXJnZXRfcGFyYW1ldGVycyI6IG51bGwsCiAgInRhc2tfdHlwZSI6IG51bGws"
    "CiAgInRyYWluYWJsZV90b2tlbl9pbmRpY2VzIjogbnVsbCwKICAidXNlX2JkbG9yYSI6IG51bGwsCiAgInVzZV9k"
    "b3JhIjogZmFsc2UsCiAgInVzZV9xYWxvcmEiOiBmYWxzZSwKICAidXNlX3JzbG9yYSI6IGZhbHNlCn0="
)
LIVE_CONFIG_BYTES = base64.b64decode(_LIVE_CONFIG_B64)


class Fixture:
    def __init__(self, root: Path):
        self.root = root
        self.peft = root / "peft"
        self.peft.mkdir()
        self.base = root / "omniASR-LLM-7B-v2.pt"
        self.adapter = self.peft / "adapter_model.safetensors"
        self.config = self.peft / "adapter_config.json"
        self.tokenizer = root / "omniASR_tokenizer_written_v2.model"
        self.out = root / "deployment_manifest.json"
        self.base.write_bytes(b"small test stand-in for the 30-GB base")
        self.adapter.write_bytes(b"small test stand-in for the private LoRA")
        self.config.write_bytes(LIVE_CONFIG_BYTES)
        self.tokenizer.write_bytes(b"small test stand-in for the tokenizer")

    def fake_pinned_hasher(self, path: Path, label: str):
        resolved = path.resolve(strict=True)
        sha = {
            self.base.resolve(): bootstrap.EXPECTED_BASE_SHA256,
            self.adapter.resolve(): bootstrap.EXPECTED_ADAPTER_SHA256,
            self.tokenizer.resolve(): bootstrap.EXPECTED_TOKENIZER_SHA256,
        }.get(resolved)
        if sha is None:
            sha = hashlib.sha256(resolved.read_bytes()).hexdigest()
        return resolved, resolved.stat().st_size, sha

    def build(self) -> dict:
        with mock.patch.object(bootstrap, "_sha256_file", side_effect=self.fake_pinned_hasher):
            return bootstrap.build_manifest(
                base=self.base,
                adapter=self.adapter,
                adapter_config=self.config,
                tokenizer=self.tokenizer,
                measurement_evidence=MEASUREMENT_EVIDENCE,
                out=self.out,
            )


class LegacyBootstrapTests(unittest.TestCase):
    def test_real_config_and_measurement_fixture_hashes_are_pinned(self) -> None:
        self.assertEqual(hashlib.sha256(LIVE_CONFIG_BYTES).hexdigest(), EXPECTED_ADAPTER_CONFIG_SHA256)
        self.assertTrue(MEASUREMENT_EVIDENCE.is_file())
        self.assertEqual(
            hashlib.sha256(MEASUREMENT_EVIDENCE.read_bytes()).hexdigest(),
            EXPECTED_MEASUREMENT_EVIDENCE_SHA256,
        )

    def test_deterministic_canonical_manifest_and_exact_contract(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))
            first = fixture.build()
            second = fixture.build()
            first_bytes = bootstrap.manifest_file_bytes(first)
            self.assertEqual(first, second)
            self.assertEqual(first_bytes, bootstrap.manifest_file_bytes(second))
            self.assertTrue(first_bytes.endswith(b"\n"))
            self.assertNotIn(b"\r", first_bytes)
            self.assertEqual(first_bytes, bootstrap._canonical_json_bytes(first) + b"\n")
            self.assertEqual(first["model_id"], "omniasr-7b-legacy-c348ade8a816")
            self.assertEqual(first["model_card"], "soranivoice_omniASR_LLM_7B_v2_local")
            self.assertEqual(first["runtime"], {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288})
            self.assertEqual(first["language"], "ckb_Arab")
            self.assertEqual(first["base_checkpoint"]["id"], "omniASR-LLM-7B-v2")
            self.assertEqual(first["adapter_config"]["sha256"], EXPECTED_ADAPTER_CONFIG_SHA256)
            self.assertEqual(
                first["provenance"],
                {
                    "kind": "legacy_bootstrap",
                    "training_provenance": "unverifiable",
                    "measurement_evidence_sha256": EXPECTED_MEASUREMENT_EVIDENCE_SHA256,
                },
            )
            unsigned = dict(first)
            stated = unsigned.pop("manifestSha256")
            self.assertEqual(stated, hashlib.sha256(bootstrap._canonical_json_bytes(unsigned)).hexdigest())

    def test_each_recorded_model_pin_is_enforced(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))
            for target in (fixture.base, fixture.adapter, fixture.tokenizer):
                with self.subTest(target=target.name):
                    def wrong_one(path: Path, label: str, *, target=target):
                        resolved, size, sha = fixture.fake_pinned_hasher(path, label)
                        if resolved == target.resolve():
                            sha = "0" * 64
                        return resolved, size, sha

                    with mock.patch.object(bootstrap, "_sha256_file", side_effect=wrong_one):
                        with self.assertRaisesRegex(bootstrap.BootstrapError, "SHA-256 mismatch"):
                            bootstrap.build_manifest(
                                base=fixture.base,
                                adapter=fixture.adapter,
                                adapter_config=fixture.config,
                                tokenizer=fixture.tokenizer,
                                measurement_evidence=MEASUREMENT_EVIDENCE,
                                out=fixture.out,
                            )

    def test_one_byte_config_or_measurement_evidence_drift_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))

            # Leading JSON whitespace is semantically harmless and remains a valid PEFT config,
            # but it is not the measured incumbent's exact behavior-determining file.
            fixture.config.write_bytes(b" " + LIVE_CONFIG_BYTES)
            with mock.patch.object(bootstrap, "_sha256_file", side_effect=fixture.fake_pinned_hasher):
                with self.assertRaisesRegex(bootstrap.BootstrapError, "adapter_config SHA-256 mismatch"):
                    bootstrap.build_manifest(
                        base=fixture.base,
                        adapter=fixture.adapter,
                        adapter_config=fixture.config,
                        tokenizer=fixture.tokenizer,
                        measurement_evidence=MEASUREMENT_EVIDENCE,
                        out=fixture.out,
                    )

            fixture.config.write_bytes(LIVE_CONFIG_BYTES)
            changed_evidence = fixture.root / "MEASUREMENTS.changed.md"
            changed_evidence.write_bytes(MEASUREMENT_EVIDENCE.read_bytes() + b"\n")
            with mock.patch.object(bootstrap, "_sha256_file", side_effect=fixture.fake_pinned_hasher):
                with self.assertRaisesRegex(bootstrap.BootstrapError, "measurement evidence SHA-256 mismatch"):
                    bootstrap.build_manifest(
                        base=fixture.base,
                        adapter=fixture.adapter,
                        adapter_config=fixture.config,
                        tokenizer=fixture.tokenizer,
                        measurement_evidence=changed_evidence,
                        out=fixture.out,
                    )

    def test_bad_or_ambiguous_peft_config_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))
            cases = {
                "wrong rank": LIVE_CONFIG_BYTES.replace(b'"r": 16', b'"r": 8'),
                "wrong target": LIVE_CONFIG_BYTES.replace(b'"v_proj"', b'"k_proj"'),
                "wrong model": LIVE_CONFIG_BYTES.replace(b'"Wav2Vec2LlamaModel"', b'"OtherModel"'),
                "duplicate key": b'{"peft_type":"LORA","peft_type":"LORA"}',
                "not json": b"not-json",
            }
            for label, raw in cases.items():
                with self.subTest(label=label):
                    fixture.config.write_bytes(raw)
                    with mock.patch.object(bootstrap, "_sha256_file", side_effect=fixture.fake_pinned_hasher):
                        with self.assertRaises(bootstrap.BootstrapError):
                            bootstrap.build_manifest(
                                base=fixture.base,
                                adapter=fixture.adapter,
                                adapter_config=fixture.config,
                                tokenizer=fixture.tokenizer,
                                measurement_evidence=MEASUREMENT_EVIDENCE,
                                out=fixture.out,
                            )

    def test_components_must_have_safe_server_loadable_layout(self) -> None:
        with tempfile.TemporaryDirectory() as name, tempfile.TemporaryDirectory() as outside_name:
            fixture = Fixture(Path(name))
            outside = Path(outside_name) / "tokenizer.model"
            outside.write_bytes(b"outside")

            def hasher(path: Path, label: str):
                if path.resolve() == outside.resolve():
                    return outside.resolve(), outside.stat().st_size, bootstrap.EXPECTED_TOKENIZER_SHA256
                return fixture.fake_pinned_hasher(path, label)

            with mock.patch.object(bootstrap, "_sha256_file", side_effect=hasher):
                with self.assertRaisesRegex(bootstrap.BootstrapError, "tokenizer must be inside"):
                    bootstrap.build_manifest(
                        base=fixture.base,
                        adapter=fixture.adapter,
                        adapter_config=fixture.config,
                        tokenizer=outside,
                        measurement_evidence=MEASUREMENT_EVIDENCE,
                        out=fixture.out,
                    )

            renamed = fixture.peft / "wrong.safetensors"
            fixture.adapter.rename(renamed)
            with mock.patch.object(bootstrap, "_sha256_file", side_effect=lambda path, label: (
                path.resolve(), path.stat().st_size,
                bootstrap.EXPECTED_ADAPTER_SHA256 if path.resolve() == renamed.resolve()
                else fixture.fake_pinned_hasher(path, label)[2]
            )):
                with self.assertRaisesRegex(bootstrap.BootstrapError, "must be named"):
                    bootstrap.build_manifest(
                        base=fixture.base,
                        adapter=renamed,
                        adapter_config=fixture.config,
                        tokenizer=fixture.tokenizer,
                        measurement_evidence=MEASUREMENT_EVIDENCE,
                        out=fixture.out,
                    )

    def test_crlf_drift_requires_replace_and_replacement_is_canonical(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))
            canonical = bootstrap.manifest_file_bytes(fixture.build())
            fixture.out.write_bytes(canonical[:-1] + b"\r\n")
            with self.assertRaisesRegex(bootstrap.BootstrapError, "refusing to overwrite"):
                bootstrap._atomic_publish(fixture.out, canonical, replace=False)
            self.assertEqual(bootstrap._atomic_publish(fixture.out, canonical, replace=True), "replaced")
            self.assertEqual(fixture.out.read_bytes(), canonical)
            self.assertNotIn(b"\r", fixture.out.read_bytes())
            before = fixture.out.stat().st_mtime_ns
            self.assertEqual(bootstrap._atomic_publish(fixture.out, canonical, replace=False), "identical")
            self.assertEqual(fixture.out.stat().st_mtime_ns, before)

    def test_atomic_replace_failure_leaves_no_partial_output(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            out = root / "deployment.json"
            out.write_bytes(b"old-complete-file")

            # `_atomic_publish` resolves the parent (strict=True) on purpose, so `destination` is the
            # REAL path. Comparing it to the unresolved `out` fails wherever the temp dir is an alias —
            # macOS `/var` -> `/private/var`, Windows `RUNNER~1` -> long name — and the resulting
            # AssertionError masks the injected OSError, so the test fails for an environmental reason
            # instead of the behaviour it means to pin. Compare resolved to resolved.
            resolved_out = out.parent.resolve(strict=True) / out.name

            def fail_replace(source, destination):
                self.assertTrue(Path(source).is_file())
                self.assertEqual(Path(destination), resolved_out)
                raise OSError("injected replace failure")

            with self.assertRaisesRegex(bootstrap.BootstrapError, "injected replace failure"):
                bootstrap._atomic_publish(out, b"new-complete-file\n", replace=True, replace_fn=fail_replace)
            self.assertEqual(out.read_bytes(), b"old-complete-file")
            self.assertEqual(list(root.glob(".deployment.json.*.tmp")), [])

    def test_concurrent_no_replace_publish_cannot_clobber(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            out = root / "deployment.json"

            def competing_writer(source, destination):
                Path(destination).write_bytes(b"different-writer\n")
                raise FileExistsError("injected no-clobber race")

            with self.assertRaisesRegex(bootstrap.BootstrapError, "concurrently-created"):
                bootstrap._atomic_publish(
                    out,
                    b"our-manifest\n",
                    replace=False,
                    link_fn=competing_writer,
                )
            self.assertEqual(out.read_bytes(), b"different-writer\n")
            self.assertEqual(list(root.glob(".deployment.json.*.tmp")), [])

    def test_server_verifier_accepts_generator_contract_without_ml_import(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            fixture = Fixture(Path(name))
            manifest = fixture.build()
            content = bootstrap.manifest_file_bytes(manifest)
            fixture.out.write_bytes(content)
            server = _load("cortex_7b_server_legacy_bootstrap_test", SCRIPT_DIR / "cortex_7b_server.py")
            self.assertNotIn("torch", server.__dict__)

            expected_by_path = {
                fixture.base.resolve(): manifest["base_checkpoint"]["sha256"],
                fixture.adapter.resolve(): manifest["adapter"]["sha256"],
                fixture.config.resolve(): manifest["adapter_config"]["sha256"],
                fixture.tokenizer.resolve(): manifest["tokenizer"]["sha256"],
            }

            def server_hash(path: Path) -> str:
                return expected_by_path[path.resolve()]

            with mock.patch.object(server, "_sha256_file", side_effect=server_hash):
                verified = server.verify_deployment_manifest(
                    fixture.out,
                    hashlib.sha256(content).hexdigest(),
                    "omniasr-7b-legacy-c348ade8a816",
                )
                stdout = io.StringIO()
                with contextlib.redirect_stdout(stdout):
                    self.assertEqual(
                        server.main(
                            [
                                "--verify-deployment",
                                str(fixture.out),
                                "--expected-sha256",
                                hashlib.sha256(content).hexdigest(),
                                "--expected-model-id",
                                "omniasr-7b-legacy-c348ade8a816",
                            ]
                        ),
                        0,
                    )
            reply = json.loads(stdout.getvalue())
            self.assertEqual(verified.provenance_kind, "legacy_bootstrap")
            self.assertEqual(reply["status"], "verified")
            self.assertEqual(reply["manifest"]["manifestSha256"], manifest["manifestSha256"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
