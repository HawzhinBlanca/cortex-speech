#!/usr/bin/env python3
"""Regression tests for the champion server's bounded, validated socket protocol."""

import importlib.util
import contextlib
import hashlib
import io
import json
import tempfile
import threading
import time
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "cortex_7b_server_protocol", Path(__file__).parent / "cortex_7b_server.py"
)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)


class FakeConnection:
    def __init__(self, payload: bytes):
        self.payload = bytearray(payload)
        self.timeout = None

    def settimeout(self, timeout):
        self.timeout = timeout

    def recv(self, size: int) -> bytes:
        chunk = bytes(self.payload[:size])
        del self.payload[:size]
        return chunk


def _sha(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _component(root: Path, name: str, raw: bytes) -> dict:
    (root / name).write_bytes(raw)
    return {"path": name, "sha256": _sha(raw), "size_bytes": len(raw)}


def _provenance(kind: str) -> dict:
    if kind == "legacy_bootstrap":
        return {
            "kind": "legacy_bootstrap",
            "training_provenance": "unverifiable",
            "measurement_evidence_sha256": "9" * 64,
        }
    evidence = {
        "training_contract_sha256": "6" * 64,
        "trainer_evidence_sha256": "7" * 64,
        "artifact_manifest_sha256": "8" * 64,
    }
    return {
        "kind": "flywheel",
        "snapshot": {"id": "1" * 64, "root_sha256": "2" * 64},
        "training_run_id": _sha(_mod._canonical_json_bytes(evidence)),
        "trainer": {
            "command": "trainer --contract training_contract.json",
            "identity": "trainer@sha256:test",
            "recipe_sha256": "4" * 64,
            "environment_sha256": "5" * 64,
            "seed": 42,
        },
        "source_evidence": evidence,
    }


def _rewrite_manifest(path: Path, value: dict) -> None:
    value = dict(value)
    value.pop("manifestSha256", None)
    value["manifestSha256"] = _sha(_mod._canonical_json_bytes(value))
    path.write_bytes(_mod._canonical_json_bytes(value) + b"\n")


def _write_manifest(root: Path, provenance_kind: str = "flywheel") -> tuple[Path, dict]:
    base = _component(root, "base.pt", b"base")
    base["id"] = "omniasr-base-v2"
    provenance = _provenance(provenance_kind)
    model_id = (
        f"omniasr-7b-challenger-{provenance['training_run_id'][:24]}"
        if provenance_kind == "flywheel"
        else "omniasr-7b-legacy-bootstrap"
    )
    manifest = {
        "schema": 1,
        "model_id": model_id,
        "family": "omniasr-7b",
        "model_card": "soranivoice_omniASR_LLM_7B_v2_local",
        "language": "ckb_Arab",
        "runtime": {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288},
        "base_checkpoint": base,
        "adapter": _component(root, "adapter_model.safetensors", b"adapter"),
        "adapter_config": _component(root, "adapter_config.json", b"config"),
        "tokenizer": _component(root, "tokenizer.model", b"tokenizer"),
        "server_compatibility": {"protocol": "cortex-omniasr-adapter", "version": 1},
        "provenance": provenance,
    }
    path = root / "deployment_manifest.json"
    _rewrite_manifest(path, manifest)
    return path, json.loads(path.read_text(encoding="utf-8"))


def _verified(provenance_kind: str = "flywheel"):
    temp = tempfile.TemporaryDirectory()
    path, _ = _write_manifest(Path(temp.name), provenance_kind)
    return temp, _mod.verify_deployment_manifest(path)


def test_protocol_bounds_input_before_parsing() -> None:
    conn = FakeConnection(b"x" * 65)
    try:
        _mod.read_bounded_json_request(conn, max_bytes=64)
    except ValueError as exc:
        assert "exceeds 64 bytes" in str(exc)
    else:
        raise AssertionError("oversized request was accepted")
    assert 0 < conn.timeout <= _mod.REQUEST_READ_TIMEOUT_SECONDS

    for ambiguous in (b'{"op":"health","op":"health"}\n', b'{"op":"health"}'):
        try:
            _mod.read_bounded_json_request(FakeConnection(ambiguous))
        except ValueError:
            pass
        else:
            raise AssertionError("ambiguous or unterminated request was accepted")


def test_protocol_accepts_only_valid_transcription_shapes() -> None:
    body = json.dumps({"audio_path": "/tmp/clip.wav", "start_ms": 0, "end_ms": 500}).encode() + b"\n"
    parsed = _mod.read_bounded_json_request(FakeConnection(body))
    assert _mod.validate_transcription_request(parsed) == ("/tmp/clip.wav", 0, 500)

    invalid = [
        {},
        {"audio_path": ""},
        {"audio_path": "/tmp/x.wav", "start_ms": 1},
        {"audio_path": "/tmp/x.wav", "start_ms": -1, "end_ms": 2},
        {"audio_path": "/tmp/x.wav", "start_ms": 5, "end_ms": 5},
        {"audio_path": "/tmp/x.wav", "start_ms": float("nan"), "end_ms": 5},
    ]
    for request in invalid:
        try:
            _mod.validate_transcription_request(request)
        except ValueError:
            pass
        else:
            raise AssertionError(f"invalid request was accepted: {request!r}")


def test_full_composite_and_pointer_identity_are_verified() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        manifest_path, manifest = _write_manifest(root)
        deployment_sha = _sha(manifest_path.read_bytes())
        pointer = {
            "schema": 2,
            "champions": {
                "omniasr-7b": {
                    "modelVersionId": manifest["model_id"],
                    "deploymentManifestPath": str(manifest_path),
                    "deploymentSha256": deployment_sha,
                    "source": "cortex-finetuned",
                    "license": "owner-provided",
                }
            },
        }
        pointer_path = root / "champion.json"
        pointer_path.write_text(json.dumps(pointer), encoding="utf-8")

        verified = _mod.load_champion_pointer(pointer_path)
        assert verified.model_version_id == manifest["model_id"]
        assert verified.deployment_sha256 == deployment_sha
        assert verified.component_sha256 == {
            "base": manifest["base_checkpoint"]["sha256"],
            "adapter": manifest["adapter"]["sha256"],
            "adapterConfig": manifest["adapter_config"]["sha256"],
            "tokenizer": manifest["tokenizer"]["sha256"],
        }


def test_windows_registry_manifest_paths_translate_to_the_same_wsl_filesystem() -> None:
    # An arbitrary base for a PURE translation assertion — deliberately NOT a user-profile path.
    # A WSL-mounted per-user profile root is forbidden in tracked files of this public repo
    # (test_windows_repo_hygiene.py); the value here is never inspected, only joined.
    pointer_dir = Path("/mnt/c/ProgramData/cortex-speech")
    assert _mod._pointer_manifest_path(
        "C:\\models\\deployment_manifest.json", pointer_dir, running_on_windows=False
    ) == Path("/mnt/c/models/deployment_manifest.json")
    assert _mod._pointer_manifest_path(
        "\\\\wsl.localhost\\Ubuntu\\home\\ai\\deployment_manifest.json",
        pointer_dir,
        running_on_windows=False,
    ) == Path("/home/ai/deployment_manifest.json")
    assert _mod._pointer_manifest_path(
        "deployments/current.json", pointer_dir, running_on_windows=False
    ) == pointer_dir / Path("deployments/current.json")

    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        base_path = root / "base.pt"
        base_path.write_bytes(b"base")
        original = _mod._pointer_manifest_path
        translated = []
        try:
            def translate(raw_path, relative_root, running_on_windows=None):
                translated.append((raw_path, relative_root))
                return base_path

            _mod._pointer_manifest_path = translate
            verified, _ = _mod._verify_base(
                root,
                {
                    "id": "base",
                    "path": "C:\\models\\base.pt",
                    "sha256": _sha(b"base"),
                    "size_bytes": 4,
                },
            )
            assert verified == base_path.resolve()
            assert translated == [("C:\\models\\base.pt", root)]
        finally:
            _mod._pointer_manifest_path = original


def test_legacy_bootstrap_is_explicitly_accepted_but_not_disguised_as_flywheel() -> None:
    with tempfile.TemporaryDirectory() as name:
        path, _ = _write_manifest(Path(name), "legacy_bootstrap")
        verified = _mod.verify_deployment_manifest(path)
        assert verified.provenance_kind == "legacy_bootstrap"

        value = json.loads(path.read_text(encoding="utf-8"))
        value["provenance"]["training_provenance"] = "verified"
        _rewrite_manifest(path, value)
        try:
            _mod.verify_deployment_manifest(path)
        except ValueError as exc:
            assert "unverifiable" in str(exc)
        else:
            raise AssertionError("legacy bootstrap was allowed to claim verified training provenance")


def test_flywheel_run_and_model_identity_must_derive_from_source_evidence() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        path, value = _write_manifest(root)
        value["provenance"]["training_run_id"] = "0" * 64
        _rewrite_manifest(path, value)
        try:
            _mod.verify_deployment_manifest(path)
        except ValueError as exc:
            assert "training_run_id does not derive" in str(exc)
        else:
            raise AssertionError("a forged flywheel training_run_id was accepted")

        path, value = _write_manifest(root)
        value["model_id"] = "omniasr-7b-challenger-forged"
        _rewrite_manifest(path, value)
        try:
            _mod.verify_deployment_manifest(path)
        except ValueError as exc:
            assert "model_id does not derive" in str(exc)
        else:
            raise AssertionError("a forged flywheel model_id was accepted")


def test_each_behavior_determining_file_is_mandatory_and_hash_checked() -> None:
    for filename in ("base.pt", "adapter_model.safetensors", "adapter_config.json", "tokenizer.model"):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            path, _ = _write_manifest(root)
            (root / filename).write_bytes(b"tampered")
            try:
                _mod.verify_deployment_manifest(path)
            except ValueError as exc:
                assert "mismatch" in str(exc), (filename, exc)
            else:
                raise AssertionError(f"tampered {filename} was accepted")

    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        path, value = _write_manifest(root)
        value.pop("adapter_config")
        _rewrite_manifest(path, value)
        try:
            _mod.verify_deployment_manifest(path)
        except ValueError as exc:
            assert "adapter_config" in str(exc)
        else:
            raise AssertionError("deployment without adapter_config was accepted")


def test_manifest_and_pointer_substitution_are_rejected() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        path, manifest = _write_manifest(root)
        expected = _sha(path.read_bytes())
        path.write_bytes(path.read_bytes() + b" ")
        try:
            _mod.verify_deployment_manifest(path, expected)
        except ValueError as exc:
            assert "canonical" in str(exc) or "file SHA-256 mismatch" in str(exc)
        else:
            raise AssertionError("manifest byte drift was accepted")

        path, manifest = _write_manifest(root)
        pointer = {
            "schema": 2,
            "champions": {
                "omniasr-7b": {
                    "modelVersionId": manifest["model_id"],
                    "deploymentManifestPath": str(path),
                    "deploymentSha256": "0" * 64,
                }
            },
        }
        pointer_path = root / "champion.json"
        pointer_path.write_text(json.dumps(pointer), encoding="utf-8")
        try:
            _mod.load_champion_pointer(pointer_path)
        except ValueError as exc:
            assert "file SHA-256 mismatch" in str(exc)
        else:
            raise AssertionError("pointer-to-manifest SHA mismatch was accepted")


def test_health_reports_verified_identity_without_invoking_transcription() -> None:
    temp, deployment = _verified()
    try:
        calls = []

        def transcribe(*args):
            calls.append(args)
            return "must not run"

        reply = _mod.dispatch_request({"op": "health"}, transcribe, deployment, "gpu0", threading.Lock())
        assert calls == []
        assert reply["status"] == "ready"
        assert "manifest" not in reply
        assert reply["modelVersionId"] == deployment.model_version_id
        assert reply["deploymentSha256"] == deployment.deployment_sha256
    finally:
        temp.cleanup()


def test_health_remains_responsive_while_inference_lock_is_held() -> None:
    temp, deployment = _verified()
    try:
        started = threading.Event()
        release = threading.Event()
        errors = []
        lock = threading.Lock()

        def slow_transcribe(*_args):
            started.set()
            if not release.wait(2):
                raise RuntimeError("test release timed out")
            return "دەق"

        def run_transcription():
            try:
                _mod.dispatch_request(
                    {"op": "transcribe", "audio_path": "/tmp/a.wav"},
                    slow_transcribe,
                    deployment,
                    "gpu0",
                    lock,
                )
            except Exception as exc:  # pragma: no cover - reported by assertion below
                errors.append(exc)

        thread = threading.Thread(target=run_transcription)
        thread.start()
        assert started.wait(1)
        busy_began = time.monotonic()
        try:
            _mod.dispatch_request(
                {"op": "transcribe", "audio_path": "/tmp/b.wav"},
                slow_transcribe,
                deployment,
                "gpu0",
                lock,
            )
        except _mod.ReplicaBusy:
            pass
        else:
            raise AssertionError("a second transcript queued behind one replica instead of reconnecting")
        assert time.monotonic() - busy_began < 0.2
        began = time.monotonic()
        health = _mod.dispatch_request({"op": "health"}, slow_transcribe, deployment, "gpu0", lock)
        elapsed = time.monotonic() - began
        release.set()
        thread.join(2)
        assert not thread.is_alive()
        assert errors == []
        assert health["status"] == "ready"
        assert elapsed < 0.2, f"health waited behind inference for {elapsed:.3f}s"
    finally:
        temp.cleanup()


def test_transcript_reply_carries_the_same_verified_identity() -> None:
    temp, deployment = _verified()
    try:
        reply = _mod.dispatch_request(
            {"audio_path": "/tmp/a.wav", "start_ms": 0, "end_ms": 100},
            lambda *_args: "دەقی دروست",
            deployment,
            "gpu1",
            threading.Lock(),
        )
        assert reply["transcript"] == "دەقی دروست"
        assert reply["modelVersionId"] == deployment.model_version_id
        assert reply["deploymentSha256"] == deployment.deployment_sha256
        assert reply["componentSha256"] == deployment.component_sha256
    finally:
        temp.cleanup()


def test_verify_only_cli_returns_identity_without_binding_or_loading_ml() -> None:
    with tempfile.TemporaryDirectory() as name:
        path, manifest = _write_manifest(Path(name))
        expected = _sha(path.read_bytes())
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = _mod.main(
                [
                    "--verify-deployment",
                    str(path),
                    "--expected-sha256",
                    expected,
                    "--expected-model-id",
                    manifest["model_id"],
                ]
            )
        assert code == 0
        reply = json.loads(output.getvalue())
        assert reply["status"] == "verified"
        assert reply["deploymentSha256"] == expected
        assert reply["manifest"] == manifest


@contextlib.contextmanager
def _devices_env(spec, free):
    """Run parse_device_indices with CORTEX_7B_DEVICES=spec and nvidia-smi mocked to `free`."""
    saved_env = {key: _mod.os.environ.get(key) for key in ("CORTEX_7B_DEVICES", "CUDA_DEVICE_ORDER")}
    saved_free = _mod.free_vram_mib
    calls = []

    def fake_free():
        calls.append(1)
        if isinstance(free, Exception):
            raise free
        return dict(free)

    try:
        _mod.os.environ.pop("CUDA_DEVICE_ORDER", None)
        if spec is None:
            _mod.os.environ.pop("CORTEX_7B_DEVICES", None)
        else:
            _mod.os.environ["CORTEX_7B_DEVICES"] = spec
        _mod.free_vram_mib = fake_free
        yield calls
    finally:
        _mod.free_vram_mib = saved_free
        for key, value in saved_env.items():
            if value is None:
                _mod.os.environ.pop(key, None)
            else:
                _mod.os.environ[key] = value


def _refusal(spec, free) -> str:
    with _devices_env(spec, free):
        try:
            _mod.parse_device_indices()
        except SystemExit as exc:
            return str(exc)
    raise AssertionError(f"CORTEX_7B_DEVICES={spec!r} with free={free!r} was not refused")


def test_default_device_is_gpu1_only_and_pins_pci_bus_order() -> None:
    idle = {0: 24000, 1: 24000}
    for spec in (None, "", "1", " 1 ", "1,1"):
        with _devices_env(spec, idle) as calls:
            assert _mod.parse_device_indices() == [1], spec
            assert _mod.os.environ["CUDA_DEVICE_ORDER"] == "PCI_BUS_ID"
            assert calls == [1], "the free-VRAM floor must be checked for explicit and default GPU 1"
    with _devices_env("cpu", idle) as calls:
        assert _mod.parse_device_indices() == []
        assert calls == []
        assert _mod.os.environ["CUDA_DEVICE_ORDER"] == "PCI_BUS_ID"


def test_gpu0_is_refused_even_when_named_explicitly() -> None:
    idle = {0: 24000, 1: 24000}
    for spec in ("0", "0,1", "1,0", "2", "0, 1"):
        message = _refusal(spec, idle)
        assert "GPU 1 only" in message and "GPU 0" in message, message
    assert "not a GPU index list" in _refusal("gpu1", idle)


def test_busy_or_missing_gpu1_refuses_and_never_seizes() -> None:
    floor = _mod.MIN_FREE_VRAM_MIB
    for spec in (None, "1"):
        message = _refusal(spec, {0: 24000, 1: floor - 1})
        assert "refusing to seize busy GPU(s) [1]" in message, message
        assert "does not report GPU(s) [1]" in _refusal(spec, {0: 24000})
        assert "GPU discovery failed" in _refusal(spec, RuntimeError("nvidia-smi exited 9"))
    with _devices_env(None, {0: 0, 1: floor}):
        assert _mod.parse_device_indices() == [1]


if __name__ == "__main__":
    test_default_device_is_gpu1_only_and_pins_pci_bus_order()
    test_gpu0_is_refused_even_when_named_explicitly()
    test_busy_or_missing_gpu1_refuses_and_never_seizes()
    test_protocol_bounds_input_before_parsing()
    test_protocol_accepts_only_valid_transcription_shapes()
    test_full_composite_and_pointer_identity_are_verified()
    test_windows_registry_manifest_paths_translate_to_the_same_wsl_filesystem()
    test_legacy_bootstrap_is_explicitly_accepted_but_not_disguised_as_flywheel()
    test_flywheel_run_and_model_identity_must_derive_from_source_evidence()
    test_each_behavior_determining_file_is_mandatory_and_hash_checked()
    test_manifest_and_pointer_substitution_are_rejected()
    test_health_reports_verified_identity_without_invoking_transcription()
    test_health_remains_responsive_while_inference_lock_is_held()
    test_transcript_reply_carries_the_same_verified_identity()
    test_verify_only_cli_returns_identity_without_binding_or_loading_ml()
    print("PASS: champion server socket protocol")
