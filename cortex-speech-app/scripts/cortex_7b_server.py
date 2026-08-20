#!/usr/bin/env python3
"""Warm OmniASR-7B Champion server (TCP line protocol on 127.0.0.1:8799).

The authoritative base+LoRA+tokenizer load recipe serving the protocol that
`cortex_7b_client.py` and `scripts/scorecard_7b.py` expect. COMMITTED to the repo
(owner ship decision 2026-07-10: ship = personal use, truly reliable) so the default
engine is recoverable from a clean checkout — previously this file lived only on one
machine, a bus-factor-1 risk. The server loads only the content-addressed deployment named by
``CORTEX_7B_CHAMPION_POINTER``. The pointer pins an exact manifest; that manifest pins the base,
LoRA adapter, adapter configuration, tokenizer, runtime shape, and provenance. Every byte is
verified before the socket binds. There is deliberately no model-directory or base-only fallback.

Protocol (newline-delimited JSON, one request per connection):
  ->  {"audio_path": "/mnt/c/...wav", "start_ms": 1200, "end_ms": 4800}   (start/end optional)
  ->  {"op": "health"}
  <-  {"status":"ready","modelVersionId":"...","deploymentSha256":"...",...}
  <-  {"transcript":"...","modelVersionId":"...","deploymentSha256":"..."}
  <-  {"error": "..."}                             on any failure (nothing fabricated)

Run INSIDE WSL with the champion venv (torch, fairseq2, peft, omnilingual-asr):
  CORTEX_7B_CHAMPION_POINTER=/mnt/c/.../champion.json CORTEX_7B_PORT=8799 python cortex_7b_server.py

Multi-GPU: CORTEX_7B_DEVICES="0,1" (default: every GPU nvidia-smi reports; "cpu" forces CPU)
pre-forks ONE WORKER PROCESS per GPU. Each child pins its card via CUDA_VISIBLE_DEVICES, loads a
full model replica (~15-17 GB bf16 fits a single 24 GB card), and accept()s on the SHARED listening
socket — the kernel load-balances connections across children, so N GPUs serve N requests truly in
parallel. Separate processes, not threads, ON PURPOSE: the 7B decode is an autoregressive Python
loop, and two replica THREADS in one interpreter serialize on the GIL — measured 1.10x on 2x
3090 Ti, vs the honest parallelism of processes. A single device (or CPU) runs the same serial
loop inline, exactly like the original single-model server.
"""
import argparse
import hashlib
import json
import math
import os
import socket
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))
CONFIGURED_LANG = os.environ.get("CORTEX_7B_LANG", "ckb_Arab")
MAX_REQUEST_BYTES = int(os.environ.get("CORTEX_7B_MAX_REQUEST_BYTES", str(16 * 1024)))
MAX_RESPONSE_BYTES = int(os.environ.get("CORTEX_7B_MAX_RESPONSE_BYTES", str(1024 * 1024)))
REQUEST_READ_TIMEOUT_SECONDS = float(os.environ.get("CORTEX_7B_READ_TIMEOUT_SECONDS", "10"))
JOB_TIMEOUT_SECONDS = float(os.environ.get("CORTEX_7B_JOB_TIMEOUT_SECONDS", "270"))
FFMPEG_TIMEOUT_SECONDS = float(os.environ.get("CORTEX_7B_FFMPEG_TIMEOUT_SECONDS", "120"))
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_POINTER_BYTES = 1024 * 1024
MAX_CONNECTION_THREADS = min(128, max(4, int(os.environ.get("CORTEX_7B_MAX_CONNECTION_THREADS", "32"))))
DEPLOYMENT_SCHEMA = 1
POINTER_SCHEMA = 2
FAMILY = "omniasr-7b"
PROTOCOL = "cortex-omniasr-adapter"
PROTOCOL_VERSION = 1
MODEL_CARD = "soranivoice_omniASR_LLM_7B_v2_local"
RUNTIME = {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288}


class DuplicateKeyError(ValueError):
    """A duplicate JSON key makes a signed manifest ambiguous across parsers."""


class ReplicaBusy(RuntimeError):
    """This replica is decoding; reconnect so the shared socket can select another free worker."""


def _object_without_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def _parse_json_bytes(raw: bytes, label: str) -> dict:
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_object_without_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError) as exc:
        raise ValueError(f"{label} is not unambiguous UTF-8 JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def _read_bounded_file(path: Path, limit: int, label: str) -> bytes:
    try:
        stat = path.stat()
    except OSError as exc:
        raise ValueError(f"{label} is unavailable at {path}: {exc}") from exc
    if not path.is_file() or stat.st_size <= 0 or stat.st_size > limit:
        raise ValueError(f"{label} must be a non-empty file no larger than {limit} bytes")
    try:
        return path.read_bytes()
    except OSError as exc:
        raise ValueError(f"could not read {label} at {path}: {exc}") from exc


def _canonical_json_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def _sha256_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _is_sha256(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and value == value.lower()
        and all(char in "0123456789abcdef" for char in value)
    )


def _text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip() or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError(f"{label} must be a non-empty string without control characters")
    return value


def _exact_keys(value: object, required, label: str, optional=()) -> dict:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    required = set(required)
    allowed = required | set(optional)
    missing = sorted(required - set(value))
    extra = sorted(set(value) - allowed)
    if missing or extra:
        raise ValueError(f"{label} keys mismatch (missing={missing}, extra={extra})")
    return value


def _positive_size(value: object, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def _verify_file_record(path: Path, record: dict, label: str) -> Path:
    expected_sha = record.get("sha256")
    if not _is_sha256(expected_sha):
        raise ValueError(f"{label} has no canonical lowercase SHA-256")
    expected_size = _positive_size(record.get("size_bytes"), f"{label}.size_bytes")
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        raise ValueError(f"{label} file is unavailable at {path}: {exc}") from exc
    if not resolved.is_file():
        raise ValueError(f"{label} is not a regular file: {resolved}")
    actual_size = resolved.stat().st_size
    if actual_size != expected_size:
        raise ValueError(f"{label} size mismatch: expected {expected_size}, got {actual_size}")
    actual_sha = _sha256_file(resolved)
    if actual_sha != expected_sha:
        raise ValueError(f"{label} SHA-256 mismatch: expected {expected_sha}, got {actual_sha}")
    return resolved


def _verify_relative_component(root: Path, record: object, label: str) -> Path:
    record = _exact_keys(record, {"path", "sha256", "size_bytes"}, label)
    raw_path = _text(record["path"], f"{label}.path")
    if "\\" in raw_path:
        raise ValueError(f"{label}.path must use POSIX separators")
    relative = PurePosixPath(raw_path)
    if (
        relative.is_absolute()
        or relative.as_posix() != raw_path
        or any(part in ("", ".", "..") for part in relative.parts)
    ):
        raise ValueError(f"{label}.path must be a safe path relative to the deployment manifest")
    resolved_root = root.resolve(strict=True)
    candidate = _verify_file_record(resolved_root.joinpath(*relative.parts), record, label)
    try:
        candidate.relative_to(resolved_root)
    except ValueError as exc:
        raise ValueError(f"{label}.path escapes the deployment directory") from exc
    return candidate


def _verify_base(root: Path, record: object) -> tuple[Path, str]:
    record = _exact_keys(record, {"id", "path", "sha256", "size_bytes"}, "base_checkpoint")
    base_id = _text(record["id"], "base_checkpoint.id")
    raw_path = _text(record["path"], "base_checkpoint.path")
    normalized = raw_path.replace("\\", "/")
    without_extended_prefix = normalized[4:] if normalized.startswith("//?/") else normalized
    cross_platform_absolute = (
        Path(raw_path).is_absolute()
        or (len(without_extended_prefix) > 2 and without_extended_prefix[1] == ":")
        or without_extended_prefix.lower().startswith(("//wsl.localhost/", "//wsl$/"))
    )
    if cross_platform_absolute:
        candidate = _pointer_manifest_path(raw_path, root)
    else:
        if "\\" in raw_path:
            raise ValueError("relative base_checkpoint.path must use POSIX separators")
        relative = PurePosixPath(raw_path)
        if relative.as_posix() != raw_path or any(part in ("", ".", "..") for part in relative.parts):
            raise ValueError("base_checkpoint.path is unsafe")
        candidate = root.joinpath(*relative.parts)
    return _verify_file_record(candidate, record, "base_checkpoint"), base_id


def _verify_provenance(value: object, model_version_id: str) -> str:
    if not isinstance(value, dict):
        raise ValueError("provenance must be an object")
    kind = value.get("kind")
    if kind == "flywheel":
        value = _exact_keys(
            value,
            {"kind", "snapshot", "training_run_id", "trainer", "source_evidence"},
            "flywheel provenance",
        )
        if not _is_sha256(value["training_run_id"]):
            raise ValueError("flywheel provenance.training_run_id is not a SHA-256")
        snapshot = _exact_keys(value["snapshot"], {"id", "root_sha256"}, "flywheel provenance.snapshot")
        if not _is_sha256(snapshot["id"]) or not _is_sha256(snapshot["root_sha256"]):
            raise ValueError("flywheel snapshot identity is incomplete")
        trainer = _exact_keys(
            value["trainer"],
            {"command", "identity", "recipe_sha256", "environment_sha256", "seed"},
            "flywheel provenance.trainer",
        )
        _text(trainer["command"], "trainer.command")
        _text(trainer["identity"], "trainer.identity")
        if not _is_sha256(trainer["recipe_sha256"]) or not _is_sha256(trainer["environment_sha256"]):
            raise ValueError("flywheel trainer recipe/environment identity is incomplete")
        if not isinstance(trainer["seed"], int) or isinstance(trainer["seed"], bool) or trainer["seed"] < 0:
            raise ValueError("flywheel trainer.seed must be a non-negative integer")
        evidence = _exact_keys(
            value["source_evidence"],
            {"training_contract_sha256", "trainer_evidence_sha256", "artifact_manifest_sha256"},
            "flywheel provenance.source_evidence",
        )
        if any(not _is_sha256(evidence[key]) for key in evidence):
            raise ValueError("flywheel source evidence is incomplete")
        expected_run_id = _sha256_bytes(_canonical_json_bytes(evidence))
        if value["training_run_id"] != expected_run_id:
            raise ValueError("flywheel training_run_id does not derive from canonical source evidence")
        expected_model_id = f"omniasr-7b-challenger-{expected_run_id[:24]}"
        if model_version_id != expected_model_id:
            raise ValueError("flywheel model_id does not derive from training_run_id")
        return kind
    if kind == "legacy_bootstrap":
        value = _exact_keys(
            value,
            {"kind", "training_provenance", "measurement_evidence_sha256"},
            "legacy bootstrap provenance",
        )
        if value["training_provenance"] != "unverifiable":
            raise ValueError("legacy bootstrap must explicitly state training_provenance='unverifiable'")
        if not _is_sha256(value["measurement_evidence_sha256"]):
            raise ValueError("legacy bootstrap has no measurement evidence SHA-256")
        return kind
    raise ValueError(f"unsupported deployment provenance kind {kind!r}")


@dataclass(frozen=True)
class VerifiedDeployment:
    manifest: dict
    manifest_path: Path
    deployment_sha256: str
    manifest_sha256: str
    model_version_id: str
    family: str
    model_card: str
    language: str
    provenance_kind: str
    base_id: str
    base_path: Path
    adapter_path: Path
    adapter_config_path: Path
    tokenizer_path: Path
    component_sha256: dict

    def identity_fields(self, worker: str) -> dict:
        return {
            "protocol": PROTOCOL,
            "protocolVersion": PROTOCOL_VERSION,
            "family": self.family,
            "modelVersionId": self.model_version_id,
            "deploymentSha256": self.deployment_sha256,
            "manifestSha256": self.manifest_sha256,
            "componentSha256": dict(self.component_sha256),
            "language": self.language,
            "provenanceKind": self.provenance_kind,
            "worker": worker,
        }

    def identity_reply(self, worker: str, status: str = "ready") -> dict:
        return {"schema": 1, "status": status, **self.identity_fields(worker)}


def verify_deployment_manifest(
    manifest_path: Path,
    expected_deployment_sha256: str | None = None,
    expected_model_version_id: str | None = None,
) -> VerifiedDeployment:
    """Verify canonical manifest bytes, self-hash, contract, provenance, and every serving file."""
    raw = _read_bounded_file(Path(manifest_path), MAX_MANIFEST_BYTES, "deployment manifest")
    if b"\r" in raw:
        raise ValueError("deployment manifest is not canonical LF-only UTF-8")
    value = _parse_json_bytes(raw, "deployment manifest")
    if raw != _canonical_json_bytes(value) + b"\n":
        raise ValueError("deployment manifest bytes are not canonical JSON followed by one LF")
    deployment_sha256 = _sha256_bytes(raw)
    if expected_deployment_sha256 is not None:
        if not _is_sha256(expected_deployment_sha256) or deployment_sha256 != expected_deployment_sha256:
            raise ValueError(
                f"deployment manifest file SHA-256 mismatch: expected {expected_deployment_sha256}, "
                f"got {deployment_sha256}"
            )

    value = _exact_keys(
        value,
        {
            "schema", "model_id", "family", "model_card", "language", "runtime", "base_checkpoint",
            "adapter", "adapter_config", "tokenizer", "server_compatibility", "provenance", "manifestSha256",
        },
        "deployment manifest",
    )
    stated_self_hash = value["manifestSha256"]
    unsigned = dict(value)
    unsigned.pop("manifestSha256")
    actual_self_hash = _sha256_bytes(_canonical_json_bytes(unsigned))
    if not _is_sha256(stated_self_hash) or stated_self_hash != actual_self_hash:
        raise ValueError(
            f"deployment manifest self-hash mismatch: expected {stated_self_hash!r}, got {actual_self_hash}"
        )
    if type(value["schema"]) is not int or value["schema"] != DEPLOYMENT_SCHEMA or value["family"] != FAMILY:
        raise ValueError("unsupported deployment schema or family")
    model_version_id = _text(value["model_id"], "model_id")
    if expected_model_version_id is not None and model_version_id != expected_model_version_id:
        raise ValueError(
            f"deployment model identity mismatch: pointer says {expected_model_version_id!r}, "
            f"manifest says {model_version_id!r}"
        )
    model_card = _text(value["model_card"], "model_card")
    if model_card != MODEL_CARD:
        raise ValueError(f"deployment model_card must be the pinned OmniASR-7B card {MODEL_CARD!r}")
    if value["language"] != "ckb_Arab" or CONFIGURED_LANG != value["language"]:
        raise ValueError(
            f"deployment language mismatch: manifest={value['language']!r}, configured={CONFIGURED_LANG!r}"
        )
    if value["runtime"] != RUNTIME:
        raise ValueError("deployment runtime shape does not match the pinned OmniASR-7B contract")
    compatibility = _exact_keys(
        value["server_compatibility"], {"protocol", "version"}, "server_compatibility"
    )
    if type(compatibility["version"]) is not int or compatibility != {
        "protocol": PROTOCOL,
        "version": PROTOCOL_VERSION,
    }:
        raise ValueError("deployment is not compatible with this server protocol")
    provenance_kind = _verify_provenance(value["provenance"], model_version_id)

    resolved_manifest = Path(manifest_path).resolve(strict=True)
    root = resolved_manifest.parent
    base_path, base_id = _verify_base(root, value["base_checkpoint"])
    adapter_path = _verify_relative_component(root, value["adapter"], "adapter")
    adapter_config_path = _verify_relative_component(root, value["adapter_config"], "adapter_config")
    tokenizer_path = _verify_relative_component(root, value["tokenizer"], "tokenizer")
    if adapter_path.name != "adapter_model.safetensors":
        raise ValueError("adapter must be named adapter_model.safetensors for PEFT loading")
    if adapter_config_path.name != "adapter_config.json":
        raise ValueError("adapter_config must be named adapter_config.json for PEFT loading")
    if adapter_path.parent != adapter_config_path.parent:
        raise ValueError("adapter and adapter_config must share one PEFT deployment directory")

    return VerifiedDeployment(
        manifest=value,
        manifest_path=resolved_manifest,
        deployment_sha256=deployment_sha256,
        manifest_sha256=stated_self_hash,
        model_version_id=model_version_id,
        family=value["family"],
        model_card=model_card,
        language=value["language"],
        provenance_kind=provenance_kind,
        base_id=base_id,
        base_path=base_path,
        adapter_path=adapter_path,
        adapter_config_path=adapter_config_path,
        tokenizer_path=tokenizer_path,
        component_sha256={
            "base": value["base_checkpoint"]["sha256"],
            "adapter": value["adapter"]["sha256"],
            "adapterConfig": value["adapter_config"]["sha256"],
            "tokenizer": value["tokenizer"]["sha256"],
        },
    )


def _pointer_manifest_path(
    raw_path: str, pointer_dir: Path, running_on_windows: bool | None = None
) -> Path:
    """Resolve locally, or translate a Windows registry path into the WSL filesystem."""
    if running_on_windows is None:
        running_on_windows = os.name == "nt"
    if running_on_windows:
        manifest_path = Path(raw_path)
        return manifest_path if manifest_path.is_absolute() else pointer_dir / manifest_path

    normalized = raw_path.replace("\\", "/")
    if normalized.startswith("//?/"):
        normalized = normalized[4:]
    if len(normalized) > 2 and normalized[1] == ":":
        drive = normalized[0].lower()
        if drive not in "abcdefghijklmnopqrstuvwxyz":
            raise ValueError("deploymentManifestPath has an invalid Windows drive")
        return Path(f"/mnt/{drive}{normalized[2:]}")
    lowered = normalized.lower()
    for prefix in ("//wsl.localhost/", "//wsl$/"):
        if lowered.startswith(prefix):
            remainder = normalized[len(prefix):]
            parts = remainder.split("/", 1)
            if len(parts) != 2 or not parts[0] or not parts[1]:
                raise ValueError("deploymentManifestPath has an incomplete WSL UNC path")
            return Path("/" + parts[1])
    manifest_path = Path(normalized)
    return manifest_path if manifest_path.is_absolute() else pointer_dir / manifest_path


def load_champion_pointer(pointer_path: Path) -> VerifiedDeployment:
    """Resolve and verify the one OmniASR deployment named by the durable registry pointer."""
    resolved_pointer = Path(pointer_path).resolve(strict=True)
    raw = _read_bounded_file(resolved_pointer, MAX_POINTER_BYTES, "champion pointer")
    pointer = _parse_json_bytes(raw, "champion pointer")
    pointer = _exact_keys(pointer, {"schema", "champions"}, "champion pointer")
    if type(pointer["schema"]) is not int or pointer["schema"] != POINTER_SCHEMA:
        raise ValueError(f"unsupported champion pointer schema {pointer['schema']!r}")
    champions = pointer["champions"]
    if not isinstance(champions, dict):
        raise ValueError("champion pointer champions must be an object")
    entry = _exact_keys(
        champions.get(FAMILY),
        {"modelVersionId", "deploymentManifestPath", "deploymentSha256"},
        f"champion pointer {FAMILY}",
        optional={"source", "license"},
    )
    model_version_id = _text(entry["modelVersionId"], "champion pointer modelVersionId")
    manifest_text = _text(entry["deploymentManifestPath"], "champion pointer deploymentManifestPath")
    manifest_path = _pointer_manifest_path(manifest_text, resolved_pointer.parent)
    return verify_deployment_manifest(manifest_path, entry["deploymentSha256"], model_version_id)


def read_bounded_json_request(conn: socket.socket, max_bytes: int = MAX_REQUEST_BYTES) -> dict:
    """Read exactly one bounded JSON line. A slow or oversized local client must not pin a worker."""
    if max_bytes < 2:
        raise ValueError("request limit is too small")
    deadline = time.monotonic() + REQUEST_READ_TIMEOUT_SECONDS
    buf = bytearray()
    while b"\n" not in buf:
        remaining_time = deadline - time.monotonic()
        if remaining_time <= 0:
            raise TimeoutError(f"request read exceeded {REQUEST_READ_TIMEOUT_SECONDS:g}s")
        conn.settimeout(remaining_time)
        remaining = max_bytes + 1 - len(buf)
        if remaining <= 0:
            raise ValueError(f"request exceeds {max_bytes} bytes")
        chunk = conn.recv(min(4096, remaining))
        if not chunk:
            break
        buf.extend(chunk)
        if len(buf) > max_bytes:
            raise ValueError(f"request exceeds {max_bytes} bytes")
    if not buf.strip():
        raise ValueError("empty request")
    if b"\n" not in buf:
        raise ValueError("request was not newline-terminated")
    line, trailing = bytes(buf).split(b"\n", 1)
    if trailing.strip():
        raise ValueError("request contains more than one JSON line")
    return _parse_json_bytes(line, "request")


def validate_transcription_request(request: dict) -> tuple[str, object, object]:
    allowed = {"op", "audio_path", "start_ms", "end_ms"}
    extra = sorted(set(request) - allowed)
    if extra:
        raise ValueError(f"transcription request has unknown fields: {extra}")
    if request.get("op", "transcribe") != "transcribe":
        raise ValueError("request op must be 'transcribe' or omitted")
    audio_path = request.get("audio_path")
    if not isinstance(audio_path, str) or not audio_path.strip():
        raise ValueError("audio_path must be a non-empty string")
    start_ms = request.get("start_ms")
    end_ms = request.get("end_ms")
    if (start_ms is None) != (end_ms is None):
        raise ValueError("start_ms and end_ms must be provided together")
    if start_ms is not None:
        if isinstance(start_ms, bool) or isinstance(end_ms, bool):
            raise ValueError("clip offsets must be finite numbers")
        if not isinstance(start_ms, (int, float)) or not isinstance(end_ms, (int, float)):
            raise ValueError("clip offsets must be finite numbers")
        if not math.isfinite(start_ms) or not math.isfinite(end_ms):
            raise ValueError("clip offsets must be finite numbers")
        if start_ms < 0 or end_ms <= start_ms:
            raise ValueError("clip offsets must satisfy 0 <= start_ms < end_ms")
    return audio_path, start_ms, end_ms


def dispatch_request(
    request: dict,
    transcribe,
    deployment: VerifiedDeployment,
    worker_tag: str,
    inference_lock,
) -> dict:
    """Route one parsed request. Health never waits for the per-replica inference lock."""
    if request.get("op") == "health":
        if set(request) != {"op"}:
            raise ValueError("health request accepts only the 'op' field")
        return deployment.identity_reply(worker_tag)
    audio_path, start_ms, end_ms = validate_transcription_request(request)
    if not inference_lock.acquire(blocking=False):
        raise ReplicaBusy("replica busy; retry on another worker")
    try:
        watchdog = threading.Timer(
            JOB_TIMEOUT_SECONDS,
            lambda: (
                print(
                    f"[{worker_tag}] FATAL: job exceeded {JOB_TIMEOUT_SECONDS:.0f}s; killing wedged worker",
                    flush=True,
                ),
                os._exit(124),
            ),
        )
        watchdog.daemon = True
        watchdog.start()
        try:
            text = transcribe(audio_path, start_ms, end_ms)
        finally:
            watchdog.cancel()
    finally:
        inference_lock.release()
    if not isinstance(text, str):
        raise RuntimeError("ASR pipeline returned a non-string transcript")
    return {"transcript": text, **deployment.identity_fields(worker_tag)}


def encode_reply(reply: dict) -> bytes:
    payload = (json.dumps(reply, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    if len(payload) > MAX_RESPONSE_BYTES:
        raise RuntimeError(f"server reply exceeds {MAX_RESPONSE_BYTES} bytes")
    return payload


# Auto-discovered devices must have at least this much VRAM free, or they are not taken. The 7B
# replica needs ~19 GiB resident; grabbing a card that training already owns OOMs the training run
# and then the replica. An EXPLICIT CORTEX_7B_DEVICES list is the owner's word and skips the check.
MIN_FREE_VRAM_MIB = int(os.environ.get("CORTEX_7B_MIN_FREE_VRAM_MIB", "20000"))


def free_vram_mib() -> dict:
    """GPU index -> free VRAM in MiB, via nvidia-smi (the parent must stay CUDA-free)."""
    out = subprocess.run(
        ["nvidia-smi", "--query-gpu=index,memory.free", "--format=csv,noheader,nounits"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if out.returncode != 0:
        raise RuntimeError(f"nvidia-smi --query-gpu exited {out.returncode}")
    result = {}
    for line in out.stdout.splitlines():
        parts = [p.strip() for p in line.split(",")]
        if len(parts) == 2 and parts[0].isdigit():
            result[int(parts[0])] = int(float(parts[1]))
    return result


def parse_device_indices() -> list:
    """GPU indices to serve on — WITHOUT touching CUDA. The parent must stay CUDA-free: a forked
    child cannot safely inherit an initialized CUDA context, so device discovery goes through
    nvidia-smi and each child initializes CUDA itself after pinning CUDA_VISIBLE_DEVICES.

    FAIL LOUD, NEVER SIDEWAYS (2026-08-20 external review). Two silent hazards lived here:
      * discovery failure returned [] — which the caller reads as "CPU mode", so a broken
        nvidia-smi launched a champion that "works" at a fraction of the speed while the owner
        believes the GPUs are serving. CPU is now a mode you must ask for by name.
      * auto-discovery took EVERY card unconditionally — including one a training run already
        owns, OOMing both sides. Auto mode now takes only cards with MIN_FREE_VRAM_MIB free and
        refuses to start if none qualifies. An explicit CORTEX_7B_DEVICES list is the owner's
        word and is honoured verbatim, checks and all skipped.
    """
    spec = os.environ.get("CORTEX_7B_DEVICES", "").strip()
    if spec.lower() == "cpu":
        return []
    if spec:
        return [int(i) for i in spec.split(",") if i.strip() != ""]
    try:
        free = free_vram_mib()
    except Exception as exc:
        raise SystemExit(
            f"7B server: GPU discovery failed ({exc}). Fix nvidia-smi, set CORTEX_7B_DEVICES "
            f"explicitly, or set CORTEX_7B_DEVICES=cpu to run on CPU deliberately."
        ) from exc
    if not free:
        raise SystemExit(
            "7B server: nvidia-smi reports no GPUs. Set CORTEX_7B_DEVICES=cpu to run on CPU deliberately."
        )
    # Attribution note (measured on this rig 2026-08-20): nvidia-smi run INSIDE WSL does see
    # WSL-side VRAM (two loaded replicas read 6435 MiB free per card), so the floor catches both a
    # host-side training run and an orphaned WSL replica. Windows-side nvidia-smi is the blind one
    # (2026-08-16 pin) — this check runs inside WSL, where the numbers are honest.
    eligible = sorted(i for i, mib in free.items() if mib >= MIN_FREE_VRAM_MIB)
    busy = sorted(i for i, mib in free.items() if mib < MIN_FREE_VRAM_MIB)
    if busy:
        print(
            f"devices: skipping GPU(s) {busy} — less than {MIN_FREE_VRAM_MIB} MiB free "
            f"(another workload owns them); set CORTEX_7B_DEVICES to override",
            flush=True,
        )
    if not eligible:
        raise SystemExit(
            f"7B server: no GPU has {MIN_FREE_VRAM_MIB} MiB free "
            f"(free now: { {i: f'{m} MiB' for i, m in sorted(free.items())} }). Another workload owns the cards. "
            f"Set CORTEX_7B_DEVICES to claim specific GPUs anyway, or CORTEX_7B_DEVICES=cpu for CPU."
        )
    return eligible


def worker(srv: socket.socket, tag: str, deployment: VerifiedDeployment) -> None:
    """One full replica: heavy imports, model load, then the proven serial accept loop. Runs in its
    own process per GPU (or inline for single-device/CPU). Everything CUDA lives below this line."""
    import copy
    import tempfile

    import torch
    import torchaudio

    # These are the exact files the parent hashed before fork. No cache discovery, directory override,
    # or base-only branch is allowed: a different composition is a different model version.
    model_dir = deployment.adapter_path.parent
    tokenizer_path = deployment.tokenizer_path
    checkpoint_path = deployment.base_path
    os.environ.setdefault("FAIRSEQ2_USER_ASSET_DIR", str(deployment.manifest_path.parent))

    # --- fairseq2 / PEFT-LoRA compatibility patches (verbatim from server.py, the proven recipe) --
    try:
        from fairseq2.nn.projection import Linear as BaseLinear
        from peft.tuners.lora import Linear as LoraLinear
        from peft import PeftModel, LoraConfig
        from fairseq2.models.hub import load_model
        from fairseq2.data.tokenizers import load_tokenizer
        from fairseq2.assets import get_asset_store, load_in_memory_asset_metadata
        from fairseq2.runtime.dependency import get_dependency_resolver
        from fairseq2.runtime.config_registry import get_config
        from omnilingual_asr.models.wav2vec2_llama import Wav2Vec2LlamaConfig
        from omnilingual_asr.models.inference.pipeline import ASRInferencePipeline
    except ImportError as e:
        print(f"[{tag}] Failed to import core libraries: {e}", flush=True)
        print("Ensure torch, fairseq2, peft, and omnilingual-asr are installed (use the champion venv).")
        os._exit(1)

    BaseLinear.in_features = property(lambda self: self.input_dim)
    BaseLinear.out_features = property(lambda self: self.output_dim)

    orig_post_init = LoraConfig.__post_init__

    def patched_post_init(self):
        orig_post_init(self)
        self._register_custom_module({BaseLinear: LoraLinear})

    LoraConfig.__post_init__ = patched_post_init

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    dtype = torch.bfloat16 if device.type == "cuda" else torch.float32
    print(f"[{tag}] Device: {device} | Precision: {dtype}", flush=True)

    model_card = deployment.model_card
    print(f"[{tag}] Base checkpoint: {checkpoint_path}", flush=True)

    store = get_asset_store()
    store._metadata_providers = [p for p in store._metadata_providers if getattr(p, "_source", "") != "sorani_custom"]
    provider = load_in_memory_asset_metadata(
        "sorani_custom",
        [
            {
                "name": "soranivoice_omniASR_tokenizer_written_v2_local",
                "tokenizer_family": "char_tokenizer",
                "tokenizer": str(tokenizer_path.resolve()),
            },
            {
                "name": model_card,
                "model_family": "wav2vec2_llama",
                "model_arch": "7b",
                "checkpoint": str(checkpoint_path.resolve()),
                "tokenizer_ref": "soranivoice_omniASR_tokenizer_written_v2_local",
            },
        ],
    )
    store._metadata_providers.append(provider)

    resolver = get_dependency_resolver()
    base_config = get_config(resolver, Wav2Vec2LlamaConfig, "7b")
    config_custom = copy.deepcopy(base_config)
    config_custom.llama_config.vocab_size = 10288
    config_custom.wav2vec2_asr_config.target_vocab_size = 10288

    print(f"[{tag}] Loading base weights (~30 GB, one minute)...", flush=True)
    base_model = load_model(model_card, device=device, dtype=dtype, config=config_custom)
    print(f"[{tag}] Applying pinned LoRA adapter from {model_dir}...", flush=True)
    peft_model = PeftModel.from_pretrained(base_model, str(model_dir))
    pipeline_model = peft_model.base_model.model
    print(f"[{tag}] LoRA applied.", flush=True)

    tokenizer = load_tokenizer(model_card)
    pipeline = ASRInferencePipeline(
        model_card=None, model=pipeline_model, tokenizer=tokenizer, device=device, dtype=dtype
    )
    print(f"[{tag}] Pipeline ready.", flush=True)

    def load_any(audio_path: str, start_ms=None, end_ms=None):
        """Decode ONLY the requested interval (2026-08-20 external review), ffmpeg-seek first.

        The previous shape decoded the WHOLE source and sliced in memory — for a 9 s clip cut from a
        77-minute episode that is ~500x the work per request, every request, and it scaled with the
        SOURCE length instead of the clip length. `ffmpeg -ss <start> -t <dur>` seeks in the
        container and decodes just the window. A request with no range still decodes fully (a whole
        short clip is the legitimate case).

        ffmpeg-first here, torchaudio as the rangeless fallback: MEASURED 2026-08-10, torchaudio's
        libsndfile backend refuses MPEG-4/AAC containers the rest of the app accepts (25 clips
        silently stuck at a weaker engine). ffmpeg is already a hard dependency of the import path.
        """
        cmd = ["ffmpeg", "-nostdin", "-loglevel", "error"]
        if start_ms is not None and end_ms is not None and end_ms > start_ms:
            cmd += ["-ss", f"{start_ms / 1000.0:.3f}", "-t", f"{(end_ms - start_ms) / 1000.0:.3f}"]
        cmd += ["-i", audio_path, "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "1", "-ar", "16000", "-"]
        proc = subprocess.run(cmd, capture_output=True, timeout=FFMPEG_TIMEOUT_SECONDS)
        if proc.returncode == 0 and proc.stdout:
            samples = torch.frombuffer(bytearray(proc.stdout), dtype=torch.float32)
            return samples.unsqueeze(0), 16000, True  # already 16k mono, already clipped
        if start_ms is None and end_ms is None:
            try:
                wav, sr = torchaudio.load(audio_path)
                return wav, sr, False
            except Exception:  # noqa: BLE001 - fall through to the honest ffmpeg error below
                pass
        # FAIL, never a silent empty clip: an empty tensor would transcribe to "" and the
        # caller treats "" as a transient server-under-load result.
        raise RuntimeError(
            f"ffmpeg could not decode {audio_path}: "
            f"{(proc.stderr or b'').decode('utf-8', 'replace').strip()[:200]}"
        )

    def transcribe(audio_path: str, start_ms=None, end_ms=None) -> str:
        """Resample to 16 kHz mono (OmniASR requirement), decode only [start_ms,end_ms), transcribe."""
        wav, sr, clipped = load_any(audio_path, start_ms, end_ms)  # (channels, samples)
        if not clipped and start_ms is not None and end_ms is not None and end_ms > start_ms:
            a = max(0, int(start_ms / 1000.0 * sr))
            b = min(wav.size(1), int(end_ms / 1000.0 * sr))
            wav = wav[:, a:b]
        if sr != 16000:
            wav = torchaudio.transforms.Resample(orig_freq=sr, new_freq=16000)(wav)
            sr = 16000
        if wav.size(0) > 1:
            wav = torch.mean(wav, dim=0, keepdim=True)
        tmp = tempfile.NamedTemporaryFile(delete=False, suffix=".wav")
        tmp.close()
        try:
            torchaudio.save(tmp.name, wav, 16000)
            out = pipeline.transcribe([tmp.name], lang=[deployment.language], batch_size=1)
            return out[0] if out else ""
        finally:
            try:
                os.remove(tmp.name)
            except OSError:
                pass

    inference_lock = threading.Lock()
    connection_slots = threading.BoundedSemaphore(MAX_CONNECTION_THREADS)

    def handle_connection(conn: socket.socket) -> None:
        try:
            req = read_bounded_json_request(conn)
            reply = dispatch_request(req, transcribe, deployment, tag, inference_lock)
        except ReplicaBusy as exc:
            reply = {"error": str(exc), "code": "BUSY"}
        except Exception as e:  # never fabricate — report the failure to the caller
            reply = {"error": str(e)}
        try:
            conn.sendall(encode_reply(reply))
        except Exception as exc:
            print(f"[{tag}] reply failed: {exc}", flush=True)

    def serve_connection(conn: socket.socket) -> None:
        try:
            handle_connection(conn)
        finally:
            try:
                conn.close()
            finally:
                connection_slots.release()

    # listen() HERE, not in the parent: the port must only answer once a replica can actually
    # transcribe — the launcher's READY probe and the app's preflight are TCP connects, and a
    # parent-side listen would make them pass minutes before any model is loaded. The first
    # worker's listen() flips the shared socket to listening; later workers' calls are no-ops.
    srv.listen(MAX_CONNECTION_THREADS)
    # Connections are bounded lightweight threads so a health request remains responsive while the
    # GPU is decoding. `inference_lock` still serializes model use inside each replica; separate
    # pre-forked worker processes retain the measured cross-GPU parallelism.
    print(
        f"[{tag}] serving on {HOST}:{PORT} (lang={deployment.language}, "
        f"model={deployment.model_version_id}, deployment={deployment.deployment_sha256})",
        flush=True,
    )
    while True:
        conn, _ = srv.accept()
        if not connection_slots.acquire(blocking=False):
            try:
                conn.sendall(
                    encode_reply(
                        {"error": "server connection limit reached; retry", "code": "BUSY"}
                    )
                )
            finally:
                conn.close()
            continue
        thread = threading.Thread(target=serve_connection, args=(conn,), daemon=True)
        try:
            thread.start()
        except Exception:
            conn.close()
            connection_slots.release()
            raise


def parse_args(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--verify-deployment",
        metavar="MANIFEST",
        help="verify a deployment without importing ML libraries, binding a socket, or serving",
    )
    parser.add_argument("--expected-sha256", help="expected exact deployment-manifest file SHA-256")
    parser.add_argument("--expected-model-id", help="expected immutable model version id")
    parser.add_argument(
        "--pointer",
        help="champion pointer path (default: CORTEX_7B_CHAMPION_POINTER)",
    )
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    try:
        if args.verify_deployment:
            deployment = verify_deployment_manifest(
                Path(args.verify_deployment), args.expected_sha256, args.expected_model_id
            )
            reply = deployment.identity_reply("verifier", status="verified")
            reply["manifest"] = deployment.manifest
            print(json.dumps(reply, ensure_ascii=False))
            return 0
        if args.expected_sha256 or args.expected_model_id:
            raise ValueError("--expected-sha256/--expected-model-id require --verify-deployment")
        pointer_text = args.pointer or os.environ.get("CORTEX_7B_CHAMPION_POINTER")
        if not pointer_text:
            raise ValueError(
                "no champion pointer configured; set CORTEX_7B_CHAMPION_POINTER to the registry's champion.json"
            )
        deployment = load_champion_pointer(Path(pointer_text))
    except Exception as exc:
        print(f"FATAL: refusing to serve an unverified champion deployment: {exc}", file=sys.stderr, flush=True)
        return 2

    indices = parse_device_indices()
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((HOST, PORT))
    # Deliberately NOT listening yet — see the worker's listen() note.

    print("=" * 60, flush=True)
    print("Loading OmniASR-7B Champion (base + Kurdish LoRA)...", flush=True)
    if not indices:
        # Hide every GPU BEFORE worker() derives its device from torch.cuda.is_available() —
        # without this, "cpu" printed a CPU banner while silently loading ~16 GB onto GPU 0
        # (adversarial review 2026-07-12), defeating the documented override (e.g. keeping the
        # server off the cards during a training run).
        os.environ["CUDA_VISIBLE_DEVICES"] = ""
        print("Devices: [cpu] (no CUDA GPU reported / CORTEX_7B_DEVICES=cpu)", flush=True)
        worker(srv, "cpu", deployment)
        return 0
    if len(indices) == 1 or not hasattr(os, "fork"):
        # Single GPU (or a fork-less platform): identical to the original single-model server.
        os.environ["CUDA_VISIBLE_DEVICES"] = str(indices[0])
        print(f"Devices: [cuda GPU {indices[0]}] (single replica)", flush=True)
        worker(srv, f"gpu{indices[0]}", deployment)
        return 0

    print(f"Devices: GPUs {indices} — pre-forking one worker process per GPU", flush=True)

    def spawn_worker(i):
        pid = os.fork()
        if pid == 0:
            # Child: pin the card BEFORE any CUDA init, then never return.
            os.environ["CUDA_VISIBLE_DEVICES"] = str(i)
            worker(srv, f"gpu{i}", deployment)
            os._exit(0)
        return pid

    fleet = {}
    for i in indices:
        fleet[spawn_worker(i)] = i
    # The parent deliberately KEEPS the listen socket open (2026-08-20): a respawned worker inherits
    # it at fork time, so closing it here would cap the fleet at its first generation. The parent
    # never accept()s, so holding the fd costs nothing.
    print(f"parent: {len(fleet)} workers forked (pids {sorted(fleet)}); serving once all load.", flush=True)

    # Forward termination to the fleet: without this, killing the parent (pkill / a restart script)
    # ORPHANS the GPU replicas — they keep the listen port and ~19 GB VRAM per card, and the
    # "restart the server to restore capacity" advice fails because the new parent can't bind.
    # SIGTERM every child, then let the normal supervise loop below reap them and exit.
    import signal

    stopping = threading.Event()

    def _forward_termination(signum, _frame):
        stopping.set()  # no respawns after a deliberate shutdown
        print(f"parent: signal {signum} — terminating {len(fleet)} worker(s)", flush=True)
        for pid in list(fleet):
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass  # already gone

    signal.signal(signal.SIGTERM, _forward_termination)
    signal.signal(signal.SIGINT, _forward_termination)

    supervise_workers(fleet, len(fleet), respawn=spawn_worker, stopping=stopping)
    return 1


def supervise_workers(live, total, reap=None, exit_fn=None, respawn=None, stopping=None, respawn_budget=3, backoff=None):
    """Reap workers as they exit — and RESPAWN them, bounded (2026-08-20 external review).

    The previous contract degraded gracefully but permanently: one OOM/CUDA hiccup cost a replica
    for the whole session, "restart when convenient" being the only cure. Now a dead worker is
    respawned on its own device after a short backoff, at most `respawn_budget` times per device —
    so a transient blip heals itself while a crash-looping card stops burning forks and degrades
    loudly instead. `os.wait` returns the moment any child dies, so a worker that fails DURING
    warm-up is respawned immediately rather than the failure hiding behind the load window.

    `live` is a dict pid -> device index, MUTATED IN PLACE so the parent's signal handler always
    terminates the current generation (a set is accepted for legacy callers: no device info, so no
    respawns). `stopping` suppresses respawns after a deliberate shutdown. `reap`/`exit_fn`/
    `respawn`/`backoff` are injectable so the loop is unit-testable without real fork()/os.wait()
    (Linux-only). Exits only once every worker is gone and no respawn is owed."""
    reap = reap or os.wait
    exit_fn = exit_fn or sys.exit
    backoff = backoff or time.sleep
    if not isinstance(live, dict):
        live = {pid: None for pid in live}
    respawns_used = {}
    while live:
        pid, status = reap()
        if pid not in live:
            continue
        index = live.pop(pid)
        shutting_down = stopping is not None and stopping.is_set()
        used = respawns_used.get(index, 0)
        if respawn is not None and index is not None and not shutting_down and used < respawn_budget:
            respawns_used[index] = used + 1
            print(
                f"parent: WORKER {pid} (gpu{index}) DIED (status {status}) — "
                f"respawning ({used + 1}/{respawn_budget} for this device) after backoff.",
                flush=True,
            )
            backoff(5)
            new_pid = respawn(index)
            live[new_pid] = index
            continue
        if live:
            print(
                f"parent: WORKER {pid} DIED (status {status}) — SERVING DEGRADED on {len(live)} of "
                f"{total} replica(s)"
                + (
                    ""
                    if shutting_down or index is None
                    else f" (respawn budget for gpu{index} exhausted after {used})"
                )
                + ". Restart the server when convenient to restore full capacity.",
                flush=True,
            )
        else:
            print(f"parent: last worker {pid} exited (status {status}); no replicas left — stopping.", flush=True)
    exit_fn(1)


if __name__ == "__main__":
    sys.exit(main())
