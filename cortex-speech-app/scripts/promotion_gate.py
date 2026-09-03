#!/usr/bin/env python3
"""Fail-closed promotion evidence for the fixed OmniASR-7B champion family.

The gate accepts a promotion only when the incumbent and candidate scorecards are complete,
clip-paired measurements over one exact evaluation manifest, their protected slices cover that
manifest, and cryptographically bound provenance proves what was evaluated.  The sidecar for
``results.tsv`` defaults to ``results.tsv.provenance.json`` and has this schema::

    {
      "schema_version": 1,
      "role": "champion" | "challenger",
      "scorecard_sha256": "<sha256 of the TSV bytes>",
      "snapshot_id": "<challenger snapshot this comparison adjudicates; shared context>",
      "eval_manifest_sha256": "<sha256 of --eval-manifest>",
      "eval_manifest_rows": 348,
      "model_family": "omniasr-7b",
      "model_id": "<immutable artifact identity>",
      "model_sha256": "<sha256>",
      "base_model_id": "<immutable champion base identity>",
      "base_model_sha256": "<sha256>",
      "adapter_id": "<immutable adapter identity>",
      "adapter_sha256": "<sha256>",
      "adapter_config_id": "<immutable adapter-config identity>",
      "adapter_config_sha256": "<sha256>",
      "tokenizer_id": "<immutable tokenizer identity>",
      "tokenizer_sha256": "<sha256>",
      "normalizer_id": "<immutable normalizer identity>",
      "normalizer_sha256": "<sha256>",
      "norm_basis": "<the non-empty basis repeated in every TSV row>",
      "challenger_artifact_manifest_sha256": "<trained artifact manifest sha256>",
      "challenger_deployment_manifest_sha256": "<deployment manifest file sha256>",
      "training_run_id": "<verified challenger run sha256>"
    }

Shared snapshot/eval/base/tokenizer/normalizer identities must be exactly equal.  Candidate and
incumbent model, adapter, and adapter-config identities must be different.  This deliberately makes legacy,
unbound scorecards invalid: a plausible number is not proof of which model produced it.

Usage::

    python scripts/promotion_gate.py CHAMPION.tsv CHALLENGER.tsv \
      --snapshot-id SHA256 --eval-manifest GOLD.tsv --slices eval_slices.tsv \
      --challenger-run-record RUN/challenger_run.json \
      [--champion-provenance FILE] [--challenger-provenance FILE] [--out verdict.json]

Exit 0 = PROMOTE, 1 = REJECT, 2 = INVALID evidence.  Once arguments have parsed, the program writes
a verdict even when an input is missing, malformed, or tampered.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence

from mapsswe_compare import mapsswe as canonical_mapsswe

CANONICAL_MODEL_FAMILY = "omniasr-7b"
OWNER_MODEL_CARD = "soranivoice_omniASR_LLM_7B_v2_local"
OWNER_RUNTIME = {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288}
PROVENANCE_SCHEMA_VERSION = 1
SLICE_SCHEMA_VERSION = 1
MIN_PAIRED_CLIPS = 30
MIN_SLICE_CLIPS = 5
MIN_CER_IMPROVEMENT = 0.001  # 0.1 absolute percentage points
MIN_WER_IMPROVEMENT = 0.001
MAX_P_VALUE = 0.05
SLICE_REGRESSION_TOLERANCE = 0.0
_MAX_COUNT = 2**63 - 1
_SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
_SCORE_COLUMNS = (
    "clip_index",
    "char_dist",
    "char_ref_len",
    "word_dist",
    "word_ref_len",
    "norm_basis",
)
_PROVENANCE_FIELDS = (
    "schema_version",
    "role",
    "scorecard_sha256",
    "snapshot_id",
    "eval_manifest_sha256",
    "eval_manifest_rows",
    "model_family",
    "model_id",
    "model_sha256",
    "base_model_id",
    "base_model_sha256",
    "adapter_id",
    "adapter_sha256",
    "adapter_config_id",
    "adapter_config_sha256",
    "tokenizer_id",
    "tokenizer_sha256",
    "normalizer_id",
    "normalizer_sha256",
    "norm_basis",
    "challenger_artifact_manifest_sha256",
    "challenger_deployment_manifest_sha256",
    "training_run_id",
)
_HASH_FIELDS = (
    "scorecard_sha256",
    "snapshot_id",
    "eval_manifest_sha256",
    "model_sha256",
    "base_model_sha256",
    "adapter_sha256",
    "adapter_config_sha256",
    "tokenizer_sha256",
    "normalizer_sha256",
    "challenger_artifact_manifest_sha256",
    "challenger_deployment_manifest_sha256",
    "training_run_id",
)
_IDENTITY_FIELDS = (
    "role",
    "model_family",
    "model_id",
    "base_model_id",
    "adapter_id",
    "adapter_config_id",
    "tokenizer_id",
    "normalizer_id",
    "norm_basis",
)
_SHARED_IDENTITY_FIELDS = (
    "snapshot_id",
    "eval_manifest_sha256",
    "eval_manifest_rows",
    "model_family",
    "base_model_id",
    "base_model_sha256",
    "tokenizer_id",
    "tokenizer_sha256",
    "normalizer_id",
    "normalizer_sha256",
    "norm_basis",
    "challenger_artifact_manifest_sha256",
    "challenger_deployment_manifest_sha256",
    "training_run_id",
)
_REQUIRED_SLICE_DIMENSIONS = ("source", "speaker", "dialect", "audio")

ScoreRow = tuple[int, int, int, int]


from policy_python import sha256_file


class GateInputError(ValueError):
    """Evidence is absent, malformed, ambiguous, or internally inconsistent."""


@dataclass(frozen=True)
class EvalManifest:
    sha256: str
    row_count: int
    clip_paths: tuple[str, ...]


@dataclass(frozen=True)
class Scorecard:
    rows: Mapping[int, ScoreRow]
    norm_basis: str
    sha256: str


@dataclass(frozen=True)
class SliceFile:
    slices: Mapping[str, Sequence[int]]
    metadata: Mapping[str, str]
    sha256: str


@dataclass(frozen=True)
class ChallengerRunBinding:
    """Serving identities re-derived from one trained run and its deployment bytes."""

    snapshot_id: str
    training_run_id: str
    artifact_manifest_sha256: str
    deployment_file_sha256: str
    model_family: str
    model_id: str
    model_sha256: str
    base_model_id: str
    base_model_sha256: str
    adapter_id: str
    adapter_sha256: str
    adapter_config_id: str
    adapter_config_sha256: str
    tokenizer_id: str
    tokenizer_sha256: str


def inspect_eval_manifest(path: Path) -> EvalManifest:
    """Validate the physical row set whose indices the scorecards claim to measure."""
    try:
        raw = path.read_bytes()
        text = raw.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise GateInputError(f"cannot read eval manifest {path}: {error}") from error
    lines = text.splitlines()
    if not lines:
        raise GateInputError(f"eval manifest {path} has zero rows")
    paths: list[str] = []
    seen_paths: set[str] = set()
    for line_number, line in enumerate(lines, 1):
        if not line:
            raise GateInputError(f"eval manifest {path} has a blank row at line {line_number}")
        parts = line.split("\t")
        if len(parts) < 2:
            raise GateInputError(
                f"eval manifest {path} line {line_number} is short: expected audio<TAB>reference"
            )
        clip_path, reference = parts[0], parts[1]
        if not clip_path or clip_path != clip_path.strip():
            raise GateInputError(f"eval manifest {path} line {line_number} has an empty/padded audio path")
        if not reference.strip():
            raise GateInputError(f"eval manifest {path} line {line_number} has an empty reference")
        if clip_path in seen_paths:
            raise GateInputError(
                f"eval manifest {path} repeats audio path {clip_path!r} at line {line_number}"
            )
        seen_paths.add(clip_path)
        paths.append(clip_path)
    return EvalManifest(hashlib.sha256(raw).hexdigest(), len(lines), tuple(paths))


def load_scorecard(path: Path) -> Scorecard:
    """Load a strict, self-consistent, non-empty per-clip scorecard.

    Rows are never skipped.  A short row, extra field, duplicate index, non-integer, negative error
    count, zero reference, or mixed/null basis invalidates the complete measurement.
    """
    try:
        raw = path.read_bytes()
        text = raw.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise GateInputError(f"cannot read scorecard {path}: {error}") from error
    lines = text.splitlines()
    if not lines:
        raise GateInputError(f"scorecard {path} is empty")
    header = lines[0].split("\t")
    if len(header) != len(set(header)):
        raise GateInputError(f"scorecard {path} has duplicate header columns")
    missing = [name for name in _SCORE_COLUMNS if name not in header]
    if missing:
        raise GateInputError(f"scorecard {path} is missing column(s) {missing}")
    index = {name: header.index(name) for name in _SCORE_COLUMNS}
    rows: dict[int, ScoreRow] = {}
    basis: str | None = None
    for line_number, line in enumerate(lines[1:], 2):
        if not line:
            raise GateInputError(f"scorecard {path} has a blank row at line {line_number}")
        parts = line.split("\t")
        if len(parts) != len(header):
            raise GateInputError(
                f"scorecard {path} line {line_number} has {len(parts)} field(s), expected {len(header)}"
            )
        try:
            clip = int(parts[index["clip_index"]])
            char_dist = int(parts[index["char_dist"]])
            char_ref = int(parts[index["char_ref_len"]])
            word_dist = int(parts[index["word_dist"]])
            word_ref = int(parts[index["word_ref_len"]])
        except ValueError as error:
            raise GateInputError(f"scorecard {path} line {line_number} contains a non-integer count") from error
        if clip < 0 or clip > _MAX_COUNT:
            raise GateInputError(f"scorecard {path} line {line_number} has invalid clip_index {clip}")
        if clip in rows:
            raise GateInputError(f"scorecard {path} repeats clip_index {clip} at line {line_number}")
        if char_dist < 0 or word_dist < 0:
            raise GateInputError(f"scorecard {path} line {line_number} has a negative edit distance")
        if char_ref <= 0 or word_ref <= 0:
            raise GateInputError(f"scorecard {path} line {line_number} has a non-positive reference length")
        if any(value > _MAX_COUNT for value in (char_dist, char_ref, word_dist, word_ref)):
            raise GateInputError(f"scorecard {path} line {line_number} has a count outside signed 64-bit range")
        if word_ref > char_ref:
            raise GateInputError(
                f"scorecard {path} line {line_number} has more reference words than characters"
            )
        row_basis = parts[index["norm_basis"]]
        if (
            not row_basis
            or row_basis != row_basis.strip()
            or any(ord(character) < 32 or ord(character) == 127 for character in row_basis)
        ):
            raise GateInputError(f"scorecard {path} line {line_number} has a null/padded norm_basis")
        if basis is None:
            basis = row_basis
        elif row_basis != basis:
            raise GateInputError(
                f"scorecard {path} mixes norm_basis values {basis!r} and {row_basis!r}"
            )
        rows[clip] = (char_dist, char_ref, word_dist, word_ref)
    if not rows or basis is None:
        raise GateInputError(f"scorecard {path} has zero data rows")
    return Scorecard(rows, basis, hashlib.sha256(raw).hexdigest())


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    obj: dict[str, Any] = {}
    for key, value in pairs:
        if key in obj:
            raise GateInputError(f"JSON contains duplicate key {key!r}")
        obj[key] = value
    return obj


def _reject_json_constant(value: str) -> None:
    raise GateInputError(f"JSON contains non-standard numeric constant {value}")


def load_provenance(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_strict_object,
            parse_constant=_reject_json_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateInputError(f"cannot read provenance {path}: {error}") from error
    if not isinstance(value, dict):
        raise GateInputError(f"provenance {path} must be one JSON object")
    return value


def load_slices(path: Path) -> SliceFile:
    """Load the strict, manifest-bound slice format written by build_eval_slices.py."""
    try:
        raw = path.read_bytes()
        text = raw.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise GateInputError(f"cannot read protected slices {path}: {error}") from error
    metadata: dict[str, str] = {}
    slices: dict[str, list[int]] = {}
    seen_pairs: set[tuple[str, int]] = set()
    header_seen = False
    for line_number, line in enumerate(text.splitlines(), 1):
        if not line:
            raise GateInputError(f"protected slices {path} has a blank row at line {line_number}")
        if not header_seen and line.startswith("# "):
            payload = line[2:]
            if "=" not in payload:
                raise GateInputError(f"protected slices {path} has malformed metadata at line {line_number}")
            key, value = payload.split("=", 1)
            if not key or not value or key in metadata:
                raise GateInputError(
                    f"protected slices {path} has empty/duplicate metadata at line {line_number}"
                )
            metadata[key] = value
            continue
        if not header_seen:
            if line != "slice_name\tclip_index":
                raise GateInputError(
                    f"protected slices {path} line {line_number} must be slice_name<TAB>clip_index"
                )
            header_seen = True
            continue
        if line.startswith("#"):
            raise GateInputError(f"protected slices {path} has metadata after its header at line {line_number}")
        parts = line.split("\t")
        if len(parts) != 2:
            raise GateInputError(f"protected slices {path} has a short/extra-field row at line {line_number}")
        name, raw_clip = parts
        if not name or name != name.strip():
            raise GateInputError(f"protected slices {path} line {line_number} has an empty/padded name")
        try:
            clip = int(raw_clip)
        except ValueError as error:
            raise GateInputError(f"protected slices {path} line {line_number} has non-integer clip_index") from error
        if clip < 0:
            raise GateInputError(f"protected slices {path} line {line_number} has negative clip_index")
        pair = (name, clip)
        if pair in seen_pairs:
            raise GateInputError(f"protected slices {path} repeats {name!r}/clip {clip}")
        seen_pairs.add(pair)
        slices.setdefault(name, []).append(clip)
    if not header_seen:
        raise GateInputError(f"protected slices {path} has no header")
    return SliceFile(slices, metadata, hashlib.sha256(raw).hexdigest())


def _valid_identity(value: Any) -> bool:
    return (
        isinstance(value, str)
        and bool(value)
        and value == value.strip()
        and not any(ord(character) < 32 or ord(character) == 127 for character in value)
    )


def _load_strict_json_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_strict_object,
            parse_constant=_reject_json_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateInputError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise GateInputError(f"{label} {path} must be one JSON object")
    return value


def _canonical_json_sha256(value: Mapping[str, Any]) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _run_relative_file(root: Path, metadata: Any, label: str) -> tuple[Path, str]:
    if not isinstance(metadata, dict):
        raise GateInputError(f"challenger run has no {label} metadata")
    raw_path, stated_hash = metadata.get("path"), metadata.get("sha256")
    if not isinstance(raw_path, str) or not raw_path or "\\" in raw_path:
        raise GateInputError(f"challenger run {label} path must be canonical POSIX-relative")
    relative = PurePosixPath(raw_path)
    if (
        raw_path != relative.as_posix()
        or relative.is_absolute()
        or any(part in {"", ".", ".."} or ":" in part for part in relative.parts)
    ):
        raise GateInputError(f"challenger run {label} path must be canonical POSIX-relative")
    if not isinstance(stated_hash, str) or _SHA256_RE.fullmatch(stated_hash) is None:
        raise GateInputError(f"challenger run {label} has no canonical sha256")
    try:
        path = root.joinpath(*relative.parts).resolve(strict=True)
        path.relative_to(root)
    except (FileNotFoundError, OSError, RuntimeError, ValueError) as error:
        raise GateInputError(f"challenger run {label} is missing or escapes its run directory") from error
    if not path.is_file():
        raise GateInputError(f"challenger run {label} is not a regular file")
    actual_hash = sha256_file(path)
    if actual_hash != stated_hash:
        raise GateInputError(
            f"challenger run {label} bytes do not match its sha256 ({actual_hash} != {stated_hash})"
        )
    return path, actual_hash


def _deployment_component(
    deployment: Mapping[str, Any], root: Path, name: str, *, absolute: bool = False
) -> tuple[str, str]:
    value = deployment.get(name)
    if not isinstance(value, dict):
        raise GateInputError(f"challenger deployment has no {name} component")
    raw_path, stated_hash, stated_size = value.get("path"), value.get("sha256"), value.get("size_bytes")
    if not _valid_identity(raw_path):
        raise GateInputError(f"challenger deployment {name} has no exact path identity")
    if not isinstance(stated_hash, str) or _SHA256_RE.fullmatch(stated_hash) is None:
        raise GateInputError(f"challenger deployment {name} has no canonical sha256")
    if not isinstance(stated_size, int) or isinstance(stated_size, bool) or stated_size <= 0:
        raise GateInputError(f"challenger deployment {name} has no positive size_bytes")
    try:
        if absolute:
            raw = Path(raw_path)
            if not raw.is_absolute():
                raise GateInputError(f"challenger deployment {name} path is not absolute")
            path = raw.resolve(strict=True)
        else:
            relative = PurePosixPath(raw_path)
            if (
                raw_path != relative.as_posix()
                or relative.is_absolute()
                or "\\" in raw_path
                or any(part in {"", ".", ".."} or ":" in part for part in relative.parts)
            ):
                raise GateInputError(f"challenger deployment {name} path is not canonical run-relative")
            path = root.joinpath(*relative.parts).resolve(strict=True)
            path.relative_to(root)
    except (FileNotFoundError, OSError, RuntimeError, ValueError) as error:
        raise GateInputError(f"challenger deployment {name} file is missing or unsafe") from error
    if not path.is_file():
        raise GateInputError(f"challenger deployment {name} is not a regular file")
    if path.stat().st_size != stated_size:
        raise GateInputError(f"challenger deployment {name} size differs from deployment manifest")
    actual_hash = sha256_file(path)
    if actual_hash != stated_hash:
        raise GateInputError(f"challenger deployment {name} file hash differs from deployment manifest")
    component_id = value.get("id") if name == "base_checkpoint" else raw_path
    if not _valid_identity(component_id):
        raise GateInputError(f"challenger deployment {name} has no exact id")
    return component_id, actual_hash


def load_challenger_run(path: Path) -> ChallengerRunBinding:
    """Re-derive every serving identity from one trained run; never trust sidecar claims alone."""
    record = _load_strict_json_object(path, "challenger run record")
    if record.get("schema") != 2 or record.get("status") != "trained":
        raise GateInputError("challenger run record is not schema-2 status 'trained'")
    root = path.parent.resolve(strict=True)
    snapshot_id, training_run_id = record.get("snapshot_id"), record.get("training_run_id")
    for label, value in (("snapshot_id", snapshot_id), ("training_run_id", training_run_id)):
        if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
            raise GateInputError(f"challenger run record has no canonical {label}")

    artifact_path, artifact_hash = _run_relative_file(root, record.get("artifact_manifest"), "artifact_manifest")
    if record.get("artifact_manifest_sha256") != artifact_hash:
        raise GateInputError("challenger run artifact_manifest_sha256 differs from its verified file")
    artifact_manifest = _load_strict_json_object(artifact_path, "challenger artifact manifest")

    deployment_path, deployment_file_hash = _run_relative_file(
        root, record.get("deployment_manifest"), "deployment_manifest"
    )
    deployment_metadata = record["deployment_manifest"]
    deployment = _load_strict_json_object(deployment_path, "challenger deployment manifest")
    semantic_hash = deployment.get("manifestSha256")
    if not isinstance(semantic_hash, str) or _SHA256_RE.fullmatch(semantic_hash) is None:
        raise GateInputError("challenger deployment has no canonical semantic manifestSha256")
    semantic_document = dict(deployment)
    semantic_document.pop("manifestSha256", None)
    if _canonical_json_sha256(semantic_document) != semantic_hash:
        raise GateInputError("challenger deployment canonical manifestSha256 does not match its contents")
    if deployment_metadata.get("manifestSha256") != semantic_hash:
        raise GateInputError("challenger run and deployment disagree on semantic manifestSha256")
    if deployment.get("schema") != 1 or deployment.get("family") != CANONICAL_MODEL_FAMILY:
        raise GateInputError("challenger deployment violates the owner OmniASR-7B family lock")
    if deployment.get("model_card") != OWNER_MODEL_CARD:
        raise GateInputError("challenger deployment is not the owner-pinned OmniASR-7B model card")
    if deployment.get("language") != "ckb_Arab" or deployment.get("runtime") != OWNER_RUNTIME:
        raise GateInputError("challenger deployment language/runtime is not the owner-pinned 7B contract")
    if deployment.get("server_compatibility") != {"protocol": "cortex-omniasr-adapter", "version": 1}:
        raise GateInputError("challenger deployment has an unsupported serving protocol")
    model_id = deployment.get("model_id")
    if not _valid_identity(model_id) or record.get("model_id") != model_id:
        raise GateInputError("challenger run and deployment disagree on exact model_id")
    provenance = deployment.get("provenance")
    if not isinstance(provenance, dict):
        raise GateInputError("challenger deployment has no provenance object")
    snapshot = provenance.get("snapshot")
    if not isinstance(snapshot, dict) or snapshot.get("id") != snapshot_id:
        raise GateInputError("challenger deployment snapshot differs from its run record")
    if provenance.get("training_run_id") != training_run_id:
        raise GateInputError("challenger deployment training_run_id differs from its run record")
    source_evidence = provenance.get("source_evidence")
    source_keys = {
        "training_contract_sha256",
        "trainer_evidence_sha256",
        "artifact_manifest_sha256",
    }
    if (
        not isinstance(source_evidence, dict)
        or set(source_evidence) != source_keys
        or any(
            not isinstance(source_evidence.get(key), str)
            or _SHA256_RE.fullmatch(source_evidence[key]) is None
            for key in source_keys
        )
        or source_evidence.get("artifact_manifest_sha256") != artifact_hash
    ):
        raise GateInputError("challenger deployment does not bind the verified artifact manifest")
    if _canonical_json_sha256(source_evidence) != training_run_id:
        raise GateInputError("challenger deployment training_run_id does not derive from source evidence")
    if model_id != f"omniasr-7b-challenger-{training_run_id[:24]}":
        raise GateInputError("challenger deployment model_id does not derive from training_run_id")

    base_id, base_hash = _deployment_component(deployment, root, "base_checkpoint", absolute=True)
    adapter_id, adapter_hash = _deployment_component(deployment, root, "adapter")
    tokenizer_id, tokenizer_hash = _deployment_component(deployment, root, "tokenizer")
    adapter_config_id, adapter_config_hash = _deployment_component(deployment, root, "adapter_config")
    if record.get("base_id") != base_id or record.get("base_sha256") != base_hash:
        raise GateInputError("challenger run and deployment disagree on exact base checkpoint")

    declared = artifact_manifest.get("artifacts")
    if not isinstance(declared, list):
        raise GateInputError("challenger artifact manifest has no artifacts list")
    by_role: dict[str, dict[str, Any]] = {}
    for item in declared:
        if not isinstance(item, dict) or item.get("role") not in {"adapter", "tokenizer", "adapter_config"}:
            continue
        role = item["role"]
        if role in by_role:
            raise GateInputError(f"challenger artifact manifest repeats role {role!r}")
        by_role[role] = item
    expected_components = {
        "adapter": (adapter_id, adapter_hash),
        "tokenizer": (tokenizer_id, tokenizer_hash),
        "adapter_config": (adapter_config_id, adapter_config_hash),
    }
    for role, (component_id, component_hash) in expected_components.items():
        item = by_role.get(role)
        if not isinstance(item, dict) or item.get("path") != component_id or item.get("sha256") != component_hash:
            raise GateInputError(f"challenger artifact/deployment mismatch for {role}")

    return ChallengerRunBinding(
        snapshot_id=snapshot_id,
        training_run_id=training_run_id,
        artifact_manifest_sha256=artifact_hash,
        deployment_file_sha256=deployment_file_hash,
        model_family=CANONICAL_MODEL_FAMILY,
        model_id=model_id,
        model_sha256=semantic_hash,
        base_model_id=base_id,
        base_model_sha256=base_hash,
        adapter_id=adapter_id,
        adapter_sha256=adapter_hash,
        adapter_config_id=adapter_config_id,
        adapter_config_sha256=adapter_config_hash,
        tokenizer_id=tokenizer_id,
        tokenizer_sha256=tokenizer_hash,
    )


def _provenance_problems(
    role: str,
    provenance: Mapping[str, Any],
    scorecard: Scorecard,
    expected_snapshot_id: str,
    eval_manifest_sha256: str,
    eval_manifest_rows: int,
) -> list[str]:
    problems: list[str] = []
    missing = [field for field in _PROVENANCE_FIELDS if field not in provenance]
    if missing:
        problems.append(f"{role} provenance is missing mandatory field(s) {missing}")
    if provenance.get("schema_version") != PROVENANCE_SCHEMA_VERSION or isinstance(
        provenance.get("schema_version"), bool
    ):
        problems.append(f"{role} provenance schema_version must be {PROVENANCE_SCHEMA_VERSION}")
    if provenance.get("role") != role:
        problems.append(f"{role} provenance role is {provenance.get('role')!r}")
    for field in _IDENTITY_FIELDS:
        if not _valid_identity(provenance.get(field)):
            problems.append(f"{role} provenance {field} must be a non-empty exact identity")
    for field in _HASH_FIELDS:
        value = provenance.get(field)
        if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
            problems.append(f"{role} provenance {field} must be a lowercase SHA-256")
    rows = provenance.get("eval_manifest_rows")
    if not isinstance(rows, int) or isinstance(rows, bool) or rows <= 0:
        problems.append(f"{role} provenance eval_manifest_rows must be a positive integer")
    if provenance.get("model_family") != CANONICAL_MODEL_FAMILY:
        problems.append(
            f"{role} model_family must be {CANONICAL_MODEL_FAMILY!r} (owner 7B family lock)"
        )
    expected = {
        "scorecard_sha256": scorecard.sha256,
        "snapshot_id": expected_snapshot_id,
        "eval_manifest_sha256": eval_manifest_sha256,
        "eval_manifest_rows": eval_manifest_rows,
        "norm_basis": scorecard.norm_basis,
    }
    for field, value in expected.items():
        if provenance.get(field) != value:
            problems.append(
                f"{role} provenance {field} does not match the exact evidence "
                f"({provenance.get(field)!r} != {value!r})"
            )
    return problems


def _slice_problems(slice_file: SliceFile, eval_manifest_sha256: str, eval_manifest_rows: int) -> list[str]:
    metadata = slice_file.metadata
    problems: list[str] = []
    required = (
        "schema_version",
        "eval_manifest_sha256",
        "eval_manifest_rows",
        "min_clips",
        "library_index_sha256",
    )
    missing = [field for field in required if field not in metadata]
    if missing:
        problems.append(f"protected slices metadata is missing mandatory field(s) {missing}")
    try:
        schema = int(metadata.get("schema_version", ""))
    except ValueError:
        schema = -1
    if schema != SLICE_SCHEMA_VERSION:
        problems.append(f"protected slices schema_version must be {SLICE_SCHEMA_VERSION}")
    if metadata.get("eval_manifest_sha256") != eval_manifest_sha256:
        problems.append("protected slices belong to a different eval manifest SHA-256")
    try:
        stated_rows = int(metadata.get("eval_manifest_rows", ""))
    except ValueError:
        stated_rows = -1
    if stated_rows != eval_manifest_rows:
        problems.append(
            f"protected slices state {stated_rows} manifest row(s), expected {eval_manifest_rows}"
        )
    try:
        stated_minimum = int(metadata.get("min_clips", ""))
    except ValueError:
        stated_minimum = -1
    if stated_minimum < MIN_SLICE_CLIPS:
        problems.append(
            f"protected slices min_clips is {stated_minimum}; at least {MIN_SLICE_CLIPS} is mandatory"
        )
    source_hash = metadata.get("library_index_sha256")
    if not isinstance(source_hash, str) or _SHA256_RE.fullmatch(source_hash) is None:
        problems.append("protected slices library_index_sha256 must be a lowercase SHA-256")
    if not slice_file.slices:
        problems.append("zero protected slices — no subgroup safety claim can be made")
        return problems
    expected = set(range(eval_manifest_rows))
    covered: set[int] = set()
    covered_by_dimension: dict[str, set[int]] = {
        dimension: set() for dimension in _REQUIRED_SLICE_DIMENSIONS
    }
    effective_minimum = max(MIN_SLICE_CLIPS, stated_minimum)
    for name, clips in sorted(slice_file.slices.items()):
        if not _valid_identity(name):
            problems.append(f"protected slice has an invalid name {name!r}")
        unique = set(clips)
        if len(unique) != len(clips):
            problems.append(f"protected slice {name!r} contains duplicate clip indices")
        if len(unique) < effective_minimum:
            problems.append(
                f"protected slice {name!r} has {len(unique)} clip(s); at least {effective_minimum} are mandatory"
            )
        outside = sorted(unique - expected)
        if outside:
            problems.append(f"protected slice {name!r} cites out-of-manifest clip(s) {outside[:5]}")
        covered.update(unique & expected)
        dimension = name.split(":", 1)[0]
        if dimension not in covered_by_dimension or ":" not in name:
            problems.append(
                f"protected slice {name!r} is not one of the mandatory dimensions "
                f"{list(_REQUIRED_SLICE_DIMENSIONS)}"
            )
        else:
            covered_by_dimension[dimension].update(unique & expected)
    missing_coverage = sorted(expected - covered)
    if missing_coverage:
        problems.append(
            f"protected slices leave {len(missing_coverage)} eval clip(s) unprotected "
            f"(first: {missing_coverage[:5]})"
        )
    for dimension, dimension_coverage in covered_by_dimension.items():
        missing = sorted(expected - dimension_coverage)
        if missing:
            problems.append(
                f"protected {dimension} slices leave {len(missing)} eval clip(s) unprotected "
                f"(first: {missing[:5]})"
            )
    return problems


def micro_rate(rows: Mapping[int, ScoreRow], clips: Sequence[int], metric: str) -> float | None:
    error_index, ref_index = (0, 1) if metric == "cer" else (2, 3)
    errors = sum(rows[clip][error_index] for clip in clips if clip in rows)
    refs = sum(rows[clip][ref_index] for clip in clips if clip in rows)
    return errors / refs if refs > 0 else None


def paired_significance(
    champion: Mapping[int, ScoreRow], challenger: Mapping[int, ScoreRow], clips: Sequence[int], metric: str
) -> dict[str, Any]:
    """Canonical NIST MAPSSWE, with the repo's exact sign-test fallback for zero variance."""
    error_index = 0 if metric == "cer" else 2
    diffs = [champion[clip][error_index] - challenger[clip][error_index] for clip in clips]
    mean, z_value, p_value = canonical_mapsswe(diffs)
    if math.isfinite(z_value):
        method = "mapsswe_normal"
        serializable_z: float | None = z_value
    elif mean == 0:
        method = "exact_tie"
        serializable_z = None
    else:
        method = "exact_two_sided_sign"
        serializable_z = None
    return {
        "metric": metric,
        "n": len(diffs),
        "mean_error_difference": mean,
        "z": serializable_z,
        "p_value": p_value,
        "method": method,
    }


def _run_binding_problems(
    binding: ChallengerRunBinding,
    challenger_provenance: Mapping[str, Any],
    expected_snapshot_id: str,
) -> list[str]:
    problems: list[str] = []
    for field in (
        "snapshot_id",
        "training_run_id",
        "artifact_manifest_sha256",
        "deployment_file_sha256",
        "model_sha256",
        "base_model_sha256",
        "adapter_sha256",
        "adapter_config_sha256",
        "tokenizer_sha256",
    ):
        value = getattr(binding, field)
        if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
            problems.append(f"re-verified challenger run has invalid {field}")
    for field in (
        "model_family",
        "model_id",
        "base_model_id",
        "adapter_id",
        "adapter_config_id",
        "tokenizer_id",
    ):
        if not _valid_identity(getattr(binding, field)):
            problems.append(f"re-verified challenger run has invalid {field}")
    if binding.model_family != CANONICAL_MODEL_FAMILY:
        problems.append("re-verified challenger run violates the owner OmniASR-7B family lock")
    expected = {
        "snapshot_id": binding.snapshot_id,
        "training_run_id": binding.training_run_id,
        "challenger_artifact_manifest_sha256": binding.artifact_manifest_sha256,
        "challenger_deployment_manifest_sha256": binding.deployment_file_sha256,
        "model_family": binding.model_family,
        "model_id": binding.model_id,
        "model_sha256": binding.model_sha256,
        "base_model_id": binding.base_model_id,
        "base_model_sha256": binding.base_model_sha256,
        "adapter_id": binding.adapter_id,
        "adapter_sha256": binding.adapter_sha256,
        "adapter_config_id": binding.adapter_config_id,
        "adapter_config_sha256": binding.adapter_config_sha256,
        "tokenizer_id": binding.tokenizer_id,
        "tokenizer_sha256": binding.tokenizer_sha256,
    }
    if binding.snapshot_id != expected_snapshot_id:
        problems.append("challenger run snapshot_id is not the snapshot this gate was asked to adjudicate")
    for field, value in expected.items():
        if challenger_provenance.get(field) != value:
            problems.append(
                f"challenger provenance {field} does not match the re-verified run/deployment "
                f"({challenger_provenance.get(field)!r} != {value!r})"
            )
    return problems


def decide(
    champion: Scorecard,
    challenger: Scorecard,
    champion_provenance: Mapping[str, Any],
    challenger_provenance: Mapping[str, Any],
    slice_file: SliceFile,
    challenger_run: ChallengerRunBinding,
    *,
    expected_snapshot_id: str,
    eval_manifest_sha256: str,
    eval_manifest_rows: int,
) -> dict[str, Any]:
    """Pure promotion decision.  Invalid evidence always outranks model-quality rejection."""
    reasons: list[str] = []
    verdict = "PROMOTE"

    def refuse(reason: str, *, invalid: bool = False) -> None:
        nonlocal verdict
        reasons.append(reason)
        if invalid:
            verdict = "INVALID"
        elif verdict != "INVALID":
            verdict = "REJECT"

    if not isinstance(expected_snapshot_id, str) or _SHA256_RE.fullmatch(expected_snapshot_id) is None:
        refuse("expected snapshot_id must be a lowercase SHA-256", invalid=True)
    if not isinstance(eval_manifest_sha256, str) or _SHA256_RE.fullmatch(eval_manifest_sha256) is None:
        refuse("eval manifest identity must be a lowercase SHA-256", invalid=True)
    if eval_manifest_rows <= 0:
        refuse("eval manifest has zero rows", invalid=True)

    for problem in _provenance_problems(
        "champion", champion_provenance, champion, expected_snapshot_id, eval_manifest_sha256, eval_manifest_rows
    ):
        refuse(problem, invalid=True)
    for problem in _run_binding_problems(challenger_run, challenger_provenance, expected_snapshot_id):
        refuse(problem, invalid=True)
    for problem in _provenance_problems(
        "challenger", challenger_provenance, challenger, expected_snapshot_id, eval_manifest_sha256, eval_manifest_rows
    ):
        refuse(problem, invalid=True)

    for field in _SHARED_IDENTITY_FIELDS:
        if champion_provenance.get(field) != challenger_provenance.get(field):
            refuse(
                f"champion/challenger provenance differs on shared identity {field} "
                f"({champion_provenance.get(field)!r} vs {challenger_provenance.get(field)!r})",
                invalid=True,
            )
    for field in (
        "model_id",
        "model_sha256",
        "adapter_id",
        "adapter_sha256",
        "adapter_config_id",
        "adapter_config_sha256",
    ):
        left, right = champion_provenance.get(field), challenger_provenance.get(field)
        if left == right:
            refuse(
                f"champion and challenger {field} are identical — this does not prove a distinct candidate",
                invalid=True,
            )

    if not champion.norm_basis or not challenger.norm_basis:
        refuse("normalization basis is null — cross-basis equality cannot be established", invalid=True)
    elif champion.norm_basis != challenger.norm_basis:
        refuse(
            f"normalization basis differs ({champion.norm_basis!r} vs {challenger.norm_basis!r})",
            invalid=True,
        )

    champion_ids, challenger_ids = set(champion.rows), set(challenger.rows)
    paired = sorted(champion_ids & challenger_ids)
    champion_only = sorted(champion_ids - challenger_ids)
    challenger_only = sorted(challenger_ids - champion_ids)
    if champion_only or challenger_only:
        refuse(
            f"unpaired evaluation: {len(champion_only)} champion-only and "
            f"{len(challenger_only)} challenger-only clip(s)",
            invalid=True,
        )
    expected_ids = set(range(eval_manifest_rows))
    for role, ids in (("champion", champion_ids), ("challenger", challenger_ids)):
        missing, extra = sorted(expected_ids - ids), sorted(ids - expected_ids)
        if missing or extra:
            refuse(
                f"{role} scorecard is not the exact eval manifest: {len(missing)} missing and "
                f"{len(extra)} out-of-range clip(s)",
                invalid=True,
            )
    if len(paired) < MIN_PAIRED_CLIPS:
        refuse(
            f"only {len(paired)} paired clip(s); at least {MIN_PAIRED_CLIPS} are required",
            invalid=True,
        )
    for clip in paired:
        if champion.rows[clip][1] != challenger.rows[clip][1] or champion.rows[clip][3] != challenger.rows[clip][3]:
            refuse(
                f"clip {clip} has different reference lengths between scorecards — not the same normalized truth",
                invalid=True,
            )
            break

    for problem in _slice_problems(slice_file, eval_manifest_sha256, eval_manifest_rows):
        refuse(problem, invalid=True)

    rates: dict[str, tuple[float | None, float | None]] = {}
    significance: dict[str, dict[str, Any] | None] = {}
    for metric, minimum in (("cer", MIN_CER_IMPROVEMENT), ("wer", MIN_WER_IMPROVEMENT)):
        before = micro_rate(champion.rows, paired, metric)
        after = micro_rate(challenger.rows, paired, metric)
        rates[metric] = (before, after)
        test = paired_significance(champion.rows, challenger.rows, paired, metric) if paired else None
        significance[metric] = test
        label = metric.upper()
        if before is None or after is None:
            refuse(f"{label} is undefined — no positive reference denominator", invalid=True)
            continue
        improvement = before - after
        if improvement <= 0:
            refuse(f"{label} did not improve ({before:.4f} -> {after:.4f})")
        elif improvement < minimum:
            refuse(f"{label} improved by only {improvement:.4f} (< {minimum})")
        if test is None:
            refuse(f"{label} has no paired significance estimate", invalid=True)
        elif test["p_value"] >= MAX_P_VALUE:
            refuse(
                f"{label} improvement is not significant "
                f"({test['method']} p={test['p_value']:.4f} >= {MAX_P_VALUE})"
            )

    slice_report: list[dict[str, Any]] = []
    for name, raw_clips in sorted(slice_file.slices.items()):
        clips = sorted(set(raw_clips) & champion_ids & challenger_ids)
        champion_cer = micro_rate(champion.rows, clips, "cer")
        challenger_cer = micro_rate(challenger.rows, clips, "cer")
        champion_wer = micro_rate(champion.rows, clips, "wer")
        challenger_wer = micro_rate(challenger.rows, clips, "wer")
        entry = {
            "slice": name,
            "clips": len(clips),
            "champion_cer": champion_cer,
            "challenger_cer": challenger_cer,
            "champion_wer": champion_wer,
            "challenger_wer": challenger_wer,
        }
        slice_report.append(entry)
        for metric, before, after in (
            ("CER", champion_cer, challenger_cer),
            ("WER", champion_wer, challenger_wer),
        ):
            if before is None or after is None:
                refuse(f"slice {name!r} {metric} could not be scored", invalid=True)
            elif after - before > SLICE_REGRESSION_TOLERANCE:
                refuse(
                    f"slice {name!r} {metric} REGRESSED ({before:.4f} -> {after:.4f}, "
                    f"+{after - before:.4f})"
                )

    champion_cer, challenger_cer = rates["cer"]
    champion_wer, challenger_wer = rates["wer"]
    if verdict == "PROMOTE":
        reasons.append(
            f"paired CER {champion_cer:.4f} -> {challenger_cer:.4f} and WER "
            f"{champion_wer:.4f} -> {challenger_wer:.4f} on {len(paired)} exact clips; "
            f"both significant; {len(slice_report)} protected slice(s) safe"
        )
    return {
        "schema_version": 2,
        "verdict": verdict,
        "paired_clips": len(paired),
        "champion_cer": champion_cer,
        "challenger_cer": challenger_cer,
        "champion_wer": champion_wer,
        "challenger_wer": challenger_wer,
        "cer_improvement": champion_cer - challenger_cer if champion_cer is not None and challenger_cer is not None else None,
        "wer_improvement": champion_wer - challenger_wer if champion_wer is not None and challenger_wer is not None else None,
        "significance": significance,
        "mapsswe_p": significance["cer"]["p_value"] if significance["cer"] else None,
        "norm_basis": champion.norm_basis if champion.norm_basis == challenger.norm_basis else None,
        "snapshot_id": expected_snapshot_id,
        "eval_manifest_sha256": eval_manifest_sha256,
        "eval_manifest_rows": eval_manifest_rows,
        "model_family": champion_provenance.get("model_family"),
        "challenger_artifact_manifest_sha256": challenger_provenance.get(
            "challenger_artifact_manifest_sha256"
        ),
        "challenger_deployment_manifest_sha256": challenger_provenance.get(
            "challenger_deployment_manifest_sha256"
        ),
        "training_run_id": challenger_provenance.get("training_run_id"),
        "identities": {
            "champion": {
                field: champion_provenance.get(field)
                for field in (
                    "scorecard_sha256",
                    "model_id",
                    "model_sha256",
                    "adapter_id",
                    "adapter_sha256",
                    "adapter_config_id",
                    "adapter_config_sha256",
                )
            },
            "challenger": {
                field: challenger_provenance.get(field)
                for field in (
                    "scorecard_sha256",
                    "model_id",
                    "model_sha256",
                    "adapter_id",
                    "adapter_sha256",
                    "adapter_config_id",
                    "adapter_config_sha256",
                )
            },
            "shared": {
                field: challenger_provenance.get(field)
                for field in (
                    "snapshot_id",
                    "eval_manifest_sha256",
                    "base_model_id",
                    "base_model_sha256",
                    "tokenizer_id",
                    "tokenizer_sha256",
                    "normalizer_id",
                    "normalizer_sha256",
                    "norm_basis",
                    "training_run_id",
                    "challenger_artifact_manifest_sha256",
                    "challenger_deployment_manifest_sha256",
                )
            },
        },
        "slices_sha256": slice_file.sha256,
        "slices": slice_report,
        "thresholds": {
            "min_paired_clips": MIN_PAIRED_CLIPS,
            "min_slice_clips": MIN_SLICE_CLIPS,
            "min_cer_improvement": MIN_CER_IMPROVEMENT,
            "min_wer_improvement": MIN_WER_IMPROVEMENT,
            "max_p_value_exclusive": MAX_P_VALUE,
            "slice_regression_tolerance": SLICE_REGRESSION_TOLERANCE,
        },
        "reasons": reasons,
    }


def _invalid_report(reason: str, snapshot_id: str | None = None) -> dict[str, Any]:
    return {
        "schema_version": 2,
        "verdict": "INVALID",
        "paired_clips": 0,
        "champion_cer": None,
        "challenger_cer": None,
        "champion_wer": None,
        "challenger_wer": None,
        "cer_improvement": None,
        "wer_improvement": None,
        "significance": {"cer": None, "wer": None},
        "mapsswe_p": None,
        "norm_basis": None,
        "snapshot_id": snapshot_id,
        "challenger_artifact_manifest_sha256": None,
        "challenger_deployment_manifest_sha256": None,
        "training_run_id": None,
        "identities": None,
        "slices": [],
        "reasons": [reason],
    }


def _write_json_atomic(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(value, handle, indent=2, ensure_ascii=False, allow_nan=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def _default_provenance(scorecard: Path) -> Path:
    return Path(f"{scorecard}.provenance.json")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Prove whether an OmniASR-7B challenger may be promoted")
    parser.add_argument("champion_tsv", type=Path)
    parser.add_argument("challenger_tsv", type=Path)
    parser.add_argument("--snapshot-id", required=True, help="sealed training snapshot SHA-256")
    parser.add_argument("--eval-manifest", required=True, type=Path)
    parser.add_argument("--slices", required=True, type=Path)
    parser.add_argument("--challenger-run-record", required=True, type=Path)
    parser.add_argument("--champion-provenance", type=Path)
    parser.add_argument("--challenger-provenance", type=Path)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args(argv)

    out = args.out or args.challenger_tsv.with_name("promotion_verdict.json")
    champion_provenance_path = args.champion_provenance or _default_provenance(args.champion_tsv)
    challenger_provenance_path = args.challenger_provenance or _default_provenance(args.challenger_tsv)
    try:
        manifest = inspect_eval_manifest(args.eval_manifest)
        champion = load_scorecard(args.champion_tsv)
        challenger = load_scorecard(args.challenger_tsv)
        champion_provenance = load_provenance(champion_provenance_path)
        challenger_provenance = load_provenance(challenger_provenance_path)
        slices = load_slices(args.slices)
        challenger_run = load_challenger_run(args.challenger_run_record)
        report = decide(
            champion,
            challenger,
            champion_provenance,
            challenger_provenance,
            slices,
            challenger_run,
            expected_snapshot_id=args.snapshot_id,
            eval_manifest_sha256=manifest.sha256,
            eval_manifest_rows=manifest.row_count,
        )
        report["evidence"] = {
            "champion_scorecard": str(args.champion_tsv),
            "challenger_scorecard": str(args.challenger_tsv),
            "champion_provenance": str(champion_provenance_path),
            "challenger_provenance": str(challenger_provenance_path),
            "eval_manifest": str(args.eval_manifest),
            "protected_slices": str(args.slices),
            "challenger_run_record": str(args.challenger_run_record),
        }
    except (GateInputError, OSError) as error:
        report = _invalid_report(str(error), args.snapshot_id)
        report["evidence"] = {
            "champion_scorecard": str(args.champion_tsv),
            "challenger_scorecard": str(args.challenger_tsv),
            "champion_provenance": str(champion_provenance_path),
            "challenger_provenance": str(challenger_provenance_path),
            "eval_manifest": str(args.eval_manifest),
            "protected_slices": str(args.slices),
            "challenger_run_record": str(args.challenger_run_record),
        }

    try:
        _write_json_atomic(out, report)
    except OSError as error:
        print(f"PROMOTION GATE: INVALID — could not durably write verdict {out}: {error}", file=sys.stderr)
        return 2

    print(f"PROMOTION GATE: {report['verdict']}", flush=True)
    for reason in report["reasons"]:
        print(f"  - {reason}", flush=True)
    for entry in report["slices"]:
        print(
            f"    slice {entry['slice']}: CER {entry['champion_cer']} -> {entry['challenger_cer']}; "
            f"WER {entry['champion_wer']} -> {entry['challenger_wer']} ({entry['clips']} clips)",
            flush=True,
        )
    print(f"  verdict written to {out}", flush=True)
    return {"PROMOTE": 0, "REJECT": 1}.get(report["verdict"], 2)


if __name__ == "__main__":
    sys.exit(main())
