#!/usr/bin/env python3
"""The external_offline manifest writer emits exactly what both serving verifiers and the gate accept."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import cortex_7b_server as server  # noqa: E402
import promotion_gate as gate  # noqa: E402
import write_external_deployment as writer  # noqa: E402


def _inputs(root: Path) -> dict[str, Path]:
    deploy = root / "deploy"
    deploy.mkdir()
    files = {
        "checkpoint": root / "fused_v12.pt",
        "tokenizer": deploy / "tokenizer.model",
        "parent_checkpoint": root / "fused_v11.pt",
        "training_manifest": root / "train.jsonl",
        "evidence": root / "SHA256SUMS",
    }
    for role, path in files.items():
        path.write_bytes(f"{role}-bytes".encode())
    return files


def _args(files: dict[str, Path], out: Path, *extra: str) -> list[str]:
    return [
        "--checkpoint", str(files["checkpoint"]),
        "--tokenizer", str(files["tokenizer"]),
        "--parent-checkpoint", str(files["parent_checkpoint"]),
        "--training-manifest", str(files["training_manifest"]),
        "--evidence", str(files["evidence"]),
        "--out", str(out),
        *extra,
    ]


def _run(argv: list[str]) -> tuple[int, str, str]:
    stdout, stderr = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        code = writer.main(argv)
    return code, stdout.getvalue(), stderr.getvalue()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def test_written_manifest_is_accepted_by_the_server_and_the_gate_with_measured_hashes() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        files = _inputs(root)
        out = root / "deploy" / "deployment_manifest.json"
        code, stdout, stderr = _run(_args(files, out))
        assert code == 0, stderr
        summary = json.loads(stdout)
        raw = out.read_bytes()
        verified = server.verify_deployment_manifest(out, sha(raw), summary["modelVersionId"])
        assert verified.provenance_kind == "external_offline"
        assert verified.adapter_path is None, "a fused deployment never goes through PEFT"
        assert verified.manifest["schema"] == 2
        provenance = verified.manifest["provenance"]
        assert provenance["training_manifest_sha256"] == sha(b"training_manifest-bytes")
        assert provenance["parent_checkpoint_sha256"] == sha(b"parent_checkpoint-bytes")
        assert provenance["evidence_sha256"] == sha(b"evidence-bytes")
        assert verified.manifest["base_checkpoint"]["sha256"] == sha(b"checkpoint-bytes")
        assert verified.manifest["base_checkpoint"]["id"] == "fused_v12"
        assert verified.manifest["tokenizer"]["path"] == "tokenizer.model"
        assert summary["deploymentSha256"] == sha(raw)
        binding = gate.load_deployment(out, "challenger")
        assert binding.provenance_kind == "external_offline" and binding.deployment_sha256 == sha(raw)
        record = json.loads((out.parent / writer.RECORD_NAME).read_text(encoding="utf-8"))
        assert record["deploymentSha256"] == sha(raw)
        assert record["inputs"]["evidence"]["sha256"] == provenance["evidence_sha256"]
        assert record["inputs"]["parent_checkpoint"]["path"] == str(files["parent_checkpoint"].resolve())
        # Re-running on the same bytes is idempotent; a changed input never silently overwrites.
        assert _run(_args(files, out))[0] == 0
        assert out.read_bytes() == raw
        files["evidence"].write_bytes(b"other-evidence")
        code, _, stderr = _run(_args(files, out))
        assert code == 2 and "refusing to overwrite" in stderr, stderr
        assert out.read_bytes() == raw
        assert _run(_args(files, out, "--replace"))[0] == 0
        assert out.read_bytes() != raw, "--replace publishes the re-measured identity"


def test_refuses_a_tokenizer_outside_the_deployment_and_a_model_that_is_its_own_parent() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        files = _inputs(root)
        outside = dict(files, tokenizer=root / "SHA256SUMS.tok")
        outside["tokenizer"].write_bytes(b"tok")
        code, _, stderr = _run(_args(outside, root / "deploy" / "deployment_manifest.json"))
        assert code == 2 and "inside the deployment manifest directory" in stderr, stderr
        files["parent_checkpoint"].write_bytes(b"checkpoint-bytes")
        code, _, stderr = _run(_args(files, root / "deploy" / "deployment_manifest.json"))
        assert code == 2 and "not its own parent" in stderr, stderr
        assert not (root / "deploy" / "deployment_manifest.json").exists()


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    failures = 0
    for test in tests:
        try:
            test()
        except Exception as error:  # noqa: BLE001 - report every arm
            failures += 1
            print(f"FAIL {test.__name__}: {error!r}", flush=True)
        else:
            print(f"  ok {test.__name__}", flush=True)
    if failures:
        print(f"EXTERNAL DEPLOYMENT WRITER: {failures}/{len(tests)} test(s) FAILED", flush=True)
        return 1
    print(f"EXTERNAL DEPLOYMENT WRITER: {len(tests)} arms pinned", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
