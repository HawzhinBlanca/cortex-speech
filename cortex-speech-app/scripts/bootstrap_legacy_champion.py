#!/usr/bin/env python3
"""Create the one honest deployment manifest for the pre-flywheel 7B champion.

This utility does not discover model files and does not reconstruct training provenance.  It
accepts the five explicit evidence paths, verifies every artifact hash recorded with the incumbent
measurement, structurally validates the exact PEFT configuration, and pins the exact measurement
evidence.  The result is the same canonical deployment contract used by
``train_challenger.py`` and ``cortex_7b_server.py``.

The legacy marker is intentional: the old adapter's original training manifest is unavailable, so
claiming a snapshot, trainer, or reproducible training run would fabricate provenance.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable, Sequence


EXPECTED_BASE_SHA256 = "1b29a4045ddfbe9125e6c9d465d5bc29063eea256ace37c129742edc07aed17a"
EXPECTED_ADAPTER_SHA256 = "c348ade8a8160319e7e6f070addb3c7b066b70716390e8f4ae548c7db7af3750"
EXPECTED_ADAPTER_CONFIG_SHA256 = "4b870f13ec88f4ca19cc3bdded779ec03090077a82e0381412fb80cc420ce331"
EXPECTED_TOKENIZER_SHA256 = "8aa11a1092142ef472537476ef6e76541123e2f0d789b79f3ebd119008240b1e"
EXPECTED_MEASUREMENT_EVIDENCE_SHA256 = "14793b6f9c4f67c9a08989e54212be85b56ddda06036e6ffd69e8d0f76416f88"

MODEL_ID = f"omniasr-7b-legacy-{EXPECTED_ADAPTER_SHA256[:12]}"
FAMILY = "omniasr-7b"
MODEL_CARD = "soranivoice_omniASR_LLM_7B_v2_local"
BASE_ID = "omniASR-LLM-7B-v2"
LANGUAGE = "ckb_Arab"
RUNTIME = {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288}
PROTOCOL = "cortex-omniasr-adapter"
PROTOCOL_VERSION = 1
MAX_CONFIG_BYTES = 1024 * 1024
MAX_EXISTING_MANIFEST_BYTES = 1024 * 1024


class BootstrapError(ValueError):
    """The requested bootstrap would not describe the measured incumbent exactly."""


class DuplicateKeyError(ValueError):
    """Duplicate JSON keys are ambiguous and therefore invalid evidence."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise DuplicateKeyError(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _reject_non_finite_json(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value!r} is not permitted")


def _canonical_json_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def _sha256_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _stable_file(path: Path, label: str) -> tuple[Path, int, str]:
    """Hash one explicit regular file and reject a file that changes while being read."""
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as exc:
        raise BootstrapError(f"{label} is unavailable at {path}: {exc}") from exc
    try:
        with resolved.open("rb") as handle:
            before = os.fstat(handle.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size <= 0:
                raise BootstrapError(f"{label} must be a non-empty regular file: {resolved}")
            digest = hashlib.sha256()
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
            after = os.fstat(handle.fileno())
    except BootstrapError:
        raise
    except OSError as exc:
        raise BootstrapError(f"could not read {label} at {resolved}: {exc}") from exc
    identity_before = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if identity_before != identity_after:
        raise BootstrapError(f"{label} changed while it was being hashed: {resolved}")
    return resolved, before.st_size, digest.hexdigest()


def _read_small_file(path: Path, label: str, limit: int) -> tuple[Path, bytes, str]:
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as exc:
        raise BootstrapError(f"{label} is unavailable at {path}: {exc}") from exc
    try:
        with resolved.open("rb") as handle:
            before = os.fstat(handle.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size <= 0 or before.st_size > limit:
                raise BootstrapError(
                    f"{label} must be a non-empty regular file no larger than {limit} bytes: {resolved}"
                )
            raw = handle.read(limit + 1)
            after = os.fstat(handle.fileno())
    except BootstrapError:
        raise
    except OSError as exc:
        raise BootstrapError(f"could not read {label} at {resolved}: {exc}") from exc
    if len(raw) != before.st_size:
        raise BootstrapError(f"{label} could not be read completely: {resolved}")
    identity_before = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if identity_before != identity_after:
        raise BootstrapError(f"{label} changed while it was being read: {resolved}")
    return resolved, raw, _sha256_bytes(raw)


def _sha256_file(path: Path, label: str) -> tuple[Path, int, str]:
    """Patch seam used by the no-30-GB test fixture; production always calls `_stable_file`."""
    return _stable_file(path, label)


def _finite_number(value: object, label: str, *, minimum: float, maximum: float | None = None) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BootstrapError(f"adapter_config {label} must be numeric")
    number = float(value)
    if not math.isfinite(number) or number < minimum or (maximum is not None and number > maximum):
        suffix = f"..{maximum}" if maximum is not None else " or greater"
        raise BootstrapError(f"adapter_config {label} must be {minimum}{suffix}")
    return number


def _validate_adapter_config(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=_reject_non_finite_json,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError, ValueError) as exc:
        raise BootstrapError(f"adapter_config is not unambiguous UTF-8 JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise BootstrapError("adapter_config must be a JSON object")

    # These are the load-relevant semantics of the measured Wav2Vec2-Llama PEFT adapter.  Checking
    # them catches a valid-but-unrelated LoRA config; hashing the whole file below pins every other
    # PEFT field without pretending we know the unavailable training history.
    required = {
        "peft_type",
        "auto_mapping",
        "bias",
        "r",
        "lora_alpha",
        "lora_dropout",
        "target_modules",
        "inference_mode",
        "fan_in_fan_out",
        "init_lora_weights",
    }
    missing = sorted(required - set(value))
    if missing:
        raise BootstrapError(f"adapter_config is missing required PEFT LoRA keys: {missing}")
    if value["peft_type"] != "LORA":
        raise BootstrapError("adapter_config peft_type must be 'LORA'")
    mapping = value["auto_mapping"]
    if not isinstance(mapping, dict) or mapping.get("base_model_class") != "Wav2Vec2LlamaModel" or mapping.get(
        "parent_library"
    ) != "omnilingual_asr.models.wav2vec2_llama.model":
        raise BootstrapError("adapter_config auto_mapping is not the expected OmniASR Wav2Vec2-Llama model")
    if value["bias"] != "none":
        raise BootstrapError("adapter_config bias must be 'none'")
    if isinstance(value["r"], bool) or not isinstance(value["r"], int) or value["r"] != 16:
        raise BootstrapError("adapter_config r must be the measured rank 16")
    if _finite_number(value["lora_alpha"], "lora_alpha", minimum=0.0) != 32.0:
        raise BootstrapError("adapter_config lora_alpha must be the measured value 32")
    if _finite_number(value["lora_dropout"], "lora_dropout", minimum=0.0, maximum=1.0) != 0.05:
        raise BootstrapError("adapter_config lora_dropout must be the measured value 0.05")
    targets = value["target_modules"]
    if not isinstance(targets, list) or any(not isinstance(item, str) or not item for item in targets):
        raise BootstrapError("adapter_config target_modules must be a non-empty string list")
    if len(targets) != len(set(targets)) or set(targets) != {"q_proj", "v_proj"}:
        raise BootstrapError("adapter_config target_modules must be exactly q_proj and v_proj")
    for key, expected in {
        "inference_mode": True,
        "fan_in_fan_out": False,
        "init_lora_weights": True,
    }.items():
        if type(value[key]) is not bool or value[key] is not expected:
            raise BootstrapError(f"adapter_config {key} must be {str(expected).lower()}")
    for key in ("use_dora", "use_qalora", "use_rslora"):
        if key in value and value[key] is not False:
            raise BootstrapError(f"adapter_config {key} must be false for the measured LoRA")
    return value


def _relative_component(path: Path, root: Path, label: str) -> str:
    try:
        relative = path.relative_to(root)
    except ValueError as exc:
        raise BootstrapError(f"{label} must be inside the deployment manifest directory {root}") from exc
    if not relative.parts or any(part in ("", ".", "..") for part in relative.parts):
        raise BootstrapError(f"{label} does not have a safe manifest-relative path")
    rendered = relative.as_posix()
    if rendered.startswith("/") or "\\" in rendered:
        raise BootstrapError(f"{label} does not have a portable manifest-relative path")
    return rendered


def _base_path(path: Path, root: Path) -> str:
    try:
        return _relative_component(path, root, "base checkpoint")
    except BootstrapError:
        # The server contract intentionally permits an external absolute base checkpoint (the
        # fairseq2 asset cache); the adapter/config/tokenizer remain one relocatable deployment dir.
        return str(path)


def _component(path: str, size: int, sha256: str) -> dict[str, object]:
    return {"path": path, "sha256": sha256, "size_bytes": size}


def build_manifest(
    *,
    base: Path,
    adapter: Path,
    adapter_config: Path,
    tokenizer: Path,
    measurement_evidence: Path,
    out: Path,
) -> dict[str, Any]:
    """Verify explicit incumbent inputs and return the canonical semantic manifest."""
    if not out.name or out.name in (".", ".."):
        raise BootstrapError("output must name a manifest file, not a directory")
    out_parent = out.expanduser().parent.resolve(strict=True)
    if not out_parent.is_dir():
        raise BootstrapError(f"output parent is not a directory: {out_parent}")
    resolved_out = out_parent / out.name

    base_path, base_size, base_hash = _sha256_file(base, "base checkpoint")
    adapter_path, adapter_size, adapter_hash = _sha256_file(adapter, "adapter")
    tokenizer_path, tokenizer_size, tokenizer_hash = _sha256_file(tokenizer, "tokenizer")
    config_path, config_raw, config_hash = _read_small_file(adapter_config, "adapter_config", MAX_CONFIG_BYTES)
    _validate_adapter_config(config_raw)
    evidence_path, evidence_size, evidence_hash = _sha256_file(measurement_evidence, "measurement evidence")

    expected_hashes = (
        (base_hash, EXPECTED_BASE_SHA256, "base checkpoint"),
        (adapter_hash, EXPECTED_ADAPTER_SHA256, "adapter"),
        (config_hash, EXPECTED_ADAPTER_CONFIG_SHA256, "adapter_config"),
        (tokenizer_hash, EXPECTED_TOKENIZER_SHA256, "tokenizer"),
        (evidence_hash, EXPECTED_MEASUREMENT_EVIDENCE_SHA256, "measurement evidence"),
    )
    for actual, expected, label in expected_hashes:
        if actual != expected:
            raise BootstrapError(f"{label} SHA-256 mismatch: expected {expected}, got {actual}")
    if adapter_path.name != "adapter_model.safetensors":
        raise BootstrapError("adapter must be named adapter_model.safetensors")
    if config_path.name != "adapter_config.json":
        raise BootstrapError("adapter_config must be named adapter_config.json")
    if adapter_path.parent != config_path.parent:
        raise BootstrapError("adapter and adapter_config must share one PEFT directory")

    inputs = {base_path, adapter_path, config_path, tokenizer_path, evidence_path}
    if resolved_out in inputs:
        raise BootstrapError("output path must not overwrite an input or evidence file")

    adapter_relative = _relative_component(adapter_path, out_parent, "adapter")
    config_relative = _relative_component(config_path, out_parent, "adapter_config")
    tokenizer_relative = _relative_component(tokenizer_path, out_parent, "tokenizer")

    manifest: dict[str, Any] = {
        "schema": 1,
        "model_id": MODEL_ID,
        "family": FAMILY,
        "model_card": MODEL_CARD,
        "language": LANGUAGE,
        "runtime": dict(RUNTIME),
        "base_checkpoint": {
            "id": BASE_ID,
            **_component(_base_path(base_path, out_parent), base_size, base_hash),
        },
        "adapter": _component(adapter_relative, adapter_size, adapter_hash),
        "adapter_config": _component(config_relative, len(config_raw), config_hash),
        "tokenizer": _component(tokenizer_relative, tokenizer_size, tokenizer_hash),
        "server_compatibility": {"protocol": PROTOCOL, "version": PROTOCOL_VERSION},
        "provenance": {
            "kind": "legacy_bootstrap",
            "training_provenance": "unverifiable",
            "measurement_evidence_sha256": evidence_hash,
        },
    }
    manifest["manifestSha256"] = _sha256_bytes(_canonical_json_bytes(manifest))
    return manifest


def manifest_file_bytes(manifest: dict[str, Any]) -> bytes:
    return _canonical_json_bytes(manifest) + b"\n"


def _read_existing_manifest(path: Path) -> bytes:
    try:
        info = path.lstat()
    except OSError as exc:
        raise BootstrapError(f"cannot inspect existing output {path}: {exc}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise BootstrapError(f"existing output must be a regular non-symlink file: {path}")
    if info.st_size <= 0 or info.st_size > MAX_EXISTING_MANIFEST_BYTES:
        raise BootstrapError(f"existing output has an invalid size: {path}")
    try:
        return path.read_bytes()
    except OSError as exc:
        raise BootstrapError(f"cannot read existing output {path}: {exc}") from exc


def _atomic_publish(
    path: Path,
    content: bytes,
    *,
    replace: bool,
    replace_fn: Callable[[str | bytes | os.PathLike[str] | os.PathLike[bytes], str | bytes | os.PathLike[str] | os.PathLike[bytes]], None] = os.replace,
    link_fn: Callable[[str | bytes | os.PathLike[str] | os.PathLike[bytes], str | bytes | os.PathLike[str] | os.PathLike[bytes]], None] = os.link,
) -> str:
    """Publish bytes atomically; return ``created``, ``identical``, or ``replaced``."""
    parent = path.parent.resolve(strict=True)
    destination = parent / path.name
    existed = destination.exists() or destination.is_symlink()
    existing_mode = 0o644
    if existed:
        existing = _read_existing_manifest(destination)
        if existing == content:
            return "identical"
        if not replace:
            raise BootstrapError(
                f"refusing to overwrite non-identical manifest {destination}; pass --replace explicitly"
            )
        existing_mode = stat.S_IMODE(destination.lstat().st_mode)

    descriptor = -1
    temporary: str | None = None
    try:
        descriptor, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", suffix=".tmp", dir=parent)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, existing_mode)
        if not existed and not replace:
            # `os.replace` would silently overwrite a file created after our initial existence
            # check.  A same-directory hard link is the portable no-clobber commit primitive: it
            # either publishes the already-fsynced inode or fails atomically with EEXIST.
            try:
                link_fn(temporary, destination)
            except FileExistsError:
                if _read_existing_manifest(destination) == content:
                    return "identical"
                raise BootstrapError(
                    f"refusing to overwrite concurrently-created non-identical manifest {destination}; "
                    "pass --replace explicitly"
                )
            os.unlink(temporary)
            temporary = None
        else:
            replace_fn(temporary, destination)
            temporary = None
        try:
            directory_fd = os.open(parent, os.O_RDONLY)
        except OSError:
            directory_fd = -1
        if directory_fd >= 0:
            try:
                os.fsync(directory_fd)
            except OSError:
                pass
            finally:
                os.close(directory_fd)
    except OSError as exc:
        raise BootstrapError(f"could not atomically publish manifest {destination}: {exc}") from exc
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if temporary is not None:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass
    return "replaced" if existed else "created"


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Create the exact legacy-bootstrap deployment manifest for the measured OmniASR-7B incumbent"
    )
    parser.add_argument("--base", type=Path, required=True, help="exact omniASR-LLM-7B-v2.pt")
    parser.add_argument("--adapter", type=Path, required=True, help="exact adapter_model.safetensors")
    parser.add_argument("--adapter-config", type=Path, required=True, help="exact adapter_config.json")
    parser.add_argument("--tokenizer", type=Path, required=True, help="exact tokenizer model")
    parser.add_argument("--measurement-evidence", type=Path, required=True, help="exact existing measurement evidence")
    parser.add_argument("--out", type=Path, required=True, help="deployment manifest output path")
    parser.add_argument("--replace", action="store_true", help="atomically replace a non-identical existing manifest")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        manifest = build_manifest(
            base=args.base,
            adapter=args.adapter,
            adapter_config=args.adapter_config,
            tokenizer=args.tokenizer,
            measurement_evidence=args.measurement_evidence,
            out=args.out,
        )
        content = manifest_file_bytes(manifest)
        status = _atomic_publish(args.out.expanduser(), content, replace=args.replace)
    except (BootstrapError, OSError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    summary = {
        "deploymentSha256": _sha256_bytes(content),
        "manifestSha256": manifest["manifestSha256"],
        "modelVersionId": manifest["model_id"],
        "path": str(args.out.expanduser().resolve(strict=True)),
        "status": status,
    }
    print(json.dumps(summary, sort_keys=True, separators=(",", ":"), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
