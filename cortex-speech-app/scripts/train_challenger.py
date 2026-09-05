#!/usr/bin/env python3
"""Fail-closed evidence wrapper for one sealed OmniASR-7B challenger run.

The trainer is external.  This wrapper owns the evidence boundary: exact snapshot root, exact base,
train-only materialization, trainer consumption attestation, real hashed artifacts, and a canonical
deployment manifest.  A subprocess exit code is never proof that training happened.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
from pathlib import Path, PurePosixPath
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
RUNS_DIR = REPO_ROOT.parent / "runs"
SHA256_HEX = 64
MANIFEST_NAME = "finetune_manifest.jsonl"
PROVENANCE_NAME = "pack_provenance.json"
SUMS_NAME = "SHA256SUMS"
TRAIN_MANIFEST_NAME = "train_manifest.jsonl"
TRAIN_INPUT_DIR = "training_input"
CONTRACT_NAME = "training_contract.json"
TRAINER_EVIDENCE_NAME = "trainer_evidence.json"
ARTIFACT_MANIFEST_NAME = "artifact_manifest.json"
DEPLOYMENT_MANIFEST_NAME = "deployment_manifest.json"
RUN_RECORD_NAME = "challenger_run.json"
VALID_SPLITS = {"train", "validation", "test"}
# A pack row is training data unless a HUMAN rejected it. The authority is Rust's
# `quality::is_human_rejected` — human_decision in {reject, human_reject}, or verdict == human_reject
# — which eval.rs already applies via the grade, so this is the SECOND gate, not the first.
#
# `edit` is the highest-value label in the corpus: the human read the champion draft, typed the
# correct text, and that text BECAME the row's sentence. Refusing it refused 241 of today's 429
# labels, and because pack problems are fail-closed (`if pack_problems: return 2`), a single edited
# row refused the ENTIRE challenger run. The legacy `human_*` spellings are accepted for the same
# reason the DB queries still match them: older rows carry them.
TRAINING_DECISIONS = {None, "accept", "human_accept", "edit", "human_edit"}
OMNIASR_MODEL_CARD = "soranivoice_omniASR_LLM_7B_v2_local"
CONTROL_FILES = {
    TRAIN_MANIFEST_NAME,
    CONTRACT_NAME,
    TRAINER_EVIDENCE_NAME,
    ARTIFACT_MANIFEST_NAME,
    DEPLOYMENT_MANIFEST_NAME,
    RUN_RECORD_NAME,
}


from policy_python import sha256_file
sha256_of = sha256_file


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def sealed_snapshot(snapshot_id: str) -> dict[str, Any] | None:
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        return None
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        row = con.execute(
            "SELECT name, status, config_json, created_at FROM dataset_runs WHERE id = ?1", (snapshot_id,)
        ).fetchone()
    finally:
        con.close()
    if not row:
        return None
    name, status, config_json, created_at = row
    try:
        config = json.loads(config_json)
    except (TypeError, ValueError):
        config = {"unparseable": config_json}
    return {"name": name, "status": status, "config": config, "created_at": created_at}


def is_sha256(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == SHA256_HEX
        and value == value.lower()
        and all(char in "0123456789abcdef" for char in value)
    )


def canonical_json_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def sha256_json(value: object) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def atomic_write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    staging = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    staging.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    os.replace(staging, path)


def atomic_write_canonical_json(path: Path, value: object) -> None:
    """Write exact registry bytes: canonical UTF-8 JSON, one LF, atomic replacement."""
    path.parent.mkdir(parents=True, exist_ok=True)
    staging = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    staging.write_bytes(canonical_json_bytes(value) + b"\n")
    os.replace(staging, path)


def _safe_relative(raw: object, label: str, problems: list[str]) -> PurePosixPath | None:
    if not isinstance(raw, str) or not raw:
        problems.append(f"{label} is not a non-empty relative path")
        return None
    rel = PurePosixPath(raw)
    if (
        raw != rel.as_posix()
        or "\\" in raw
        or rel.is_absolute()
        or any(part in {"", ".", ".."} for part in rel.parts)
        or any(":" in part for part in rel.parts)
    ):
        problems.append(f"{label} {raw!r} is not a canonical safe POSIX-relative path")
        return None
    return rel


def _inside(root: Path, rel: PurePosixPath, label: str, problems: list[str]) -> Path | None:
    candidate = root.joinpath(*rel.parts)
    try:
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(root)
    except (FileNotFoundError, OSError, RuntimeError, ValueError):
        problems.append(f"{label} {rel.as_posix()!r} is missing or escapes the canonical root")
        return None
    if not resolved.is_file():
        problems.append(f"{label} {rel.as_posix()!r} is not a regular file")
        return None
    return resolved


def inspect_pack(pack_dir: Path, snapshot_id: str, sealed: dict[str, Any]) -> tuple[list[str], dict[str, Any]]:
    """Validate manifest + provenance + SHA256SUMS + every referenced WAV as one root."""
    problems: list[str] = []
    evidence: dict[str, Any] = {"rows": [], "train_lines": []}
    try:
        root = pack_dir.resolve(strict=True)
    except (FileNotFoundError, OSError, RuntimeError):
        return [f"pack directory does not exist: {pack_dir}"], evidence
    if not root.is_dir():
        return [f"pack root is not a directory: {root}"], evidence
    evidence["pack_dir"] = str(root)

    if sealed.get("name") not in (None, "finetune-pack"):
        problems.append(f"snapshot name is {sealed.get('name')!r}, not 'finetune-pack'")
    if sealed.get("status") != "sealed":
        problems.append(f"snapshot status is {sealed.get('status')!r}, not 'sealed'")
    if not is_sha256(snapshot_id):
        problems.append(f"snapshot id {snapshot_id!r} is not a canonical lowercase sha256")

    manifest = root / MANIFEST_NAME
    if not manifest.is_file():
        problems.append(f"no {MANIFEST_NAME} in {root}")
        return problems, evidence
    manifest_bytes = manifest.read_bytes()
    manifest_hash = hashlib.sha256(manifest_bytes).hexdigest()
    evidence["manifest_sha256"] = manifest_hash
    if b"\r" in manifest_bytes:
        problems.append(f"{MANIFEST_NAME} contains CRLF/CR byte drift; the sealed export is canonical LF bytes")
    if manifest_hash != snapshot_id:
        problems.append(
            f"pack does not match the snapshot: manifest hashes to {manifest_hash[:16]}… but the "
            f"snapshot id is {snapshot_id[:16]}…; the pack was edited or is a different export"
        )
    try:
        manifest_text = manifest_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        problems.append(f"{MANIFEST_NAME} is not UTF-8: {error}")
        return problems, evidence

    rows: list[dict[str, Any]] = []
    raw_lines: list[str] = []
    segment_ids: set[str] = set()
    audio_paths: set[str] = set()
    for line_no, line in enumerate(manifest_text.splitlines(), start=1):
        if not line.strip():
            problems.append(f"{MANIFEST_NAME}:{line_no} is blank")
            continue
        try:
            row = json.loads(line)
        except ValueError as error:
            problems.append(f"{MANIFEST_NAME}:{line_no} is invalid JSON: {error}")
            continue
        if not isinstance(row, dict):
            problems.append(f"{MANIFEST_NAME}:{line_no} is not a JSON object")
            continue
        rows.append(row)
        raw_lines.append(line)
        sentence = row.get("sentence")
        if not isinstance(sentence, str) or not sentence.strip():
            problems.append(f"{MANIFEST_NAME}:{line_no} has no non-empty training sentence")
        duration = row.get("duration_seconds")
        if (
            not isinstance(duration, (int, float))
            or isinstance(duration, bool)
            or not math.isfinite(float(duration))
            or duration <= 0
        ):
            problems.append(f"{MANIFEST_NAME}:{line_no} has invalid duration_seconds {duration!r}")
        if not isinstance(row.get("source_recording"), str) or not row["source_recording"]:
            problems.append(f"{MANIFEST_NAME}:{line_no} has no source_recording")
        authority = row.get("review_authority")
        if authority is not None or row.get("decision") == "retain":
            valid_authority = (
                isinstance(authority, dict)
                and type(authority.get("schemaVersion")) is int
                and authority.get("schemaVersion") == 1
                and authority.get("segmentId") == row.get("segment_id")
                and authority.get("resolutionStatus") in {"resolved", "ownerResolved"}
                and authority.get("finalAction") == "retain"
                and authority.get("finalTranscript") == sentence
                and isinstance(authority.get("evidenceSha256"), str)
                and re.fullmatch(r"[0-9a-f]{64}", authority["evidenceSha256"]) is not None
                and type(authority.get("reviewerCount")) is int
                and authority["reviewerCount"] >= 2
                and row.get("decision") == "retain"
                and row.get("decision_revision_scope") == "canonical_first_opinion"
            )
            if not valid_authority:
                problems.append(f"{MANIFEST_NAME}:{line_no} has invalid final-review authority")
        elif row.get("decision") not in TRAINING_DECISIONS:
            problems.append(f"{MANIFEST_NAME}:{line_no} carries non-training decision {row.get('decision')!r}")
        revision = row.get("decision_revision")
        if not isinstance(revision, int) or isinstance(revision, bool) or revision < 0:
            problems.append(f"{MANIFEST_NAME}:{line_no} has invalid decision_revision {revision!r}")
        if row.get("grade") not in {"gold", "silver"}:
            problems.append(f"{MANIFEST_NAME}:{line_no} has non-training grade {row.get('grade')!r}")
        if not isinstance(row.get("audio_processed"), bool):
            problems.append(f"{MANIFEST_NAME}:{line_no} has invalid audio_processed provenance")
        split = row.get("split")
        if split not in VALID_SPLITS:
            problems.append(f"{MANIFEST_NAME}:{line_no} has invalid or missing split {split!r}")
        segment_id = row.get("segment_id")
        if not isinstance(segment_id, str) or not segment_id:
            problems.append(f"{MANIFEST_NAME}:{line_no} has no segment_id")
        elif segment_id in segment_ids:
            problems.append(f"{MANIFEST_NAME}:{line_no} repeats segment_id {segment_id!r}")
        else:
            segment_ids.add(segment_id)
        rel = _safe_relative(row.get("audio_path"), f"{MANIFEST_NAME}:{line_no} audio_path", problems)
        if rel is not None:
            rel_text = rel.as_posix()
            if rel.suffix.lower() != ".wav":
                problems.append(f"{MANIFEST_NAME}:{line_no} audio_path {rel_text!r} is not a WAV")
            if rel_text in audio_paths:
                problems.append(f"{MANIFEST_NAME}:{line_no} repeats audio_path {rel_text!r}")
            audio_paths.add(rel_text)

    evidence["rows"] = rows
    evidence["train_lines"] = [line for row, line in zip(rows, raw_lines) if row.get("split") == "train"]
    evidence["rows_by_split"] = {
        split: sum(row.get("split") == split for row in rows) for split in sorted(VALID_SPLITS)
    }
    evidence["rows_total"] = len(rows)
    if not rows:
        problems.append("the pack is empty — there is nothing to train on")
    if not evidence["train_lines"]:
        problems.append("no rows are in the train split — validation/test material must never be trainer input")

    provenance_path = root / PROVENANCE_NAME
    provenance: dict[str, Any] | None = None
    if not provenance_path.is_file():
        problems.append(f"{PROVENANCE_NAME} is missing")
    else:
        try:
            provenance_bytes = provenance_path.read_bytes()
            if b"\r" in provenance_bytes:
                problems.append(f"{PROVENANCE_NAME} contains CRLF/CR byte drift; canonical provenance uses LF")
            value = json.loads(provenance_bytes.decode("utf-8"))
            if not isinstance(value, dict):
                raise ValueError("top level is not an object")
            provenance = value
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"{PROVENANCE_NAME} is unreadable: {error}")
    if provenance is not None:
        if provenance.get("snapshotId") != snapshot_id:
            problems.append(f"{PROVENANCE_NAME} snapshotId does not match the sealed snapshot")
        if provenance.get("manifestSha256") != snapshot_id:
            problems.append(f"{PROVENANCE_NAME} manifestSha256 does not match the sealed snapshot")
        if provenance.get("emitted") != len(rows):
            problems.append(
                f"{PROVENANCE_NAME} emitted={provenance.get('emitted')!r} but manifest has {len(rows)} rows"
            )
        selection = sealed.get("config")
        if isinstance(selection, dict):
            for key, expected in selection.items():
                if provenance.get(key) != expected:
                    problems.append(f"{PROVENANCE_NAME} does not match sealed config field {key!r}")

    sums_path = root / SUMS_NAME
    checksums: dict[str, str] = {}
    if not sums_path.is_file():
        problems.append(f"{SUMS_NAME} is missing")
    else:
        sums_bytes = sums_path.read_bytes()
        evidence["sha256sums_sha256"] = hashlib.sha256(sums_bytes).hexdigest()
        if b"\r" in sums_bytes:
            problems.append(f"{SUMS_NAME} contains CRLF/CR byte drift; canonical records use LF")
        try:
            sums_text = sums_bytes.decode("utf-8")
        except UnicodeDecodeError as error:
            problems.append(f"{SUMS_NAME} is not UTF-8: {error}")
            sums_text = ""
        for line_no, line in enumerate(sums_text.splitlines(), start=1):
            parts = line.split("  ", 1)
            if len(parts) != 2 or not is_sha256(parts[0]):
                problems.append(f"{SUMS_NAME}:{line_no} is not '<lowercase sha256><two spaces><path>'")
                continue
            digest, raw_rel = parts
            rel = _safe_relative(raw_rel, f"{SUMS_NAME}:{line_no} path", problems)
            if rel is None:
                continue
            rel_text = rel.as_posix()
            if rel_text == SUMS_NAME:
                problems.append(f"{SUMS_NAME}:{line_no} recursively lists itself")
            if rel_text in checksums:
                problems.append(f"{SUMS_NAME}:{line_no} repeats {rel_text!r}")
            checksums[rel_text] = digest

    expected_files = {MANIFEST_NAME, PROVENANCE_NAME, *audio_paths}
    missing_sums = sorted(expected_files - checksums.keys())
    unexpected_sums = sorted(checksums.keys() - expected_files)
    if missing_sums:
        problems.append(f"{SUMS_NAME} omits {len(missing_sums)} required file(s): {', '.join(missing_sums[:5])}")
    if unexpected_sums:
        problems.append(
            f"{SUMS_NAME} lists {len(unexpected_sums)} file(s) outside canonical root: "
            f"{', '.join(unexpected_sums[:5])}"
        )

    verified_files: list[dict[str, Any]] = []
    for rel_text in sorted(expected_files):
        actual_path = _inside(root, PurePosixPath(rel_text), "snapshot file", problems)
        if actual_path is None:
            continue
        actual_hash = sha256_of(actual_path)
        expected_hash = checksums.get(rel_text)
        if expected_hash is not None and actual_hash != expected_hash:
            problems.append(
                f"{rel_text} no longer matches {SUMS_NAME} ({actual_hash[:16]}… != {expected_hash[:16]}…)"
            )
        verified_files.append({"path": rel_text, "sha256": actual_hash, "size_bytes": actual_path.stat().st_size})

    evidence["files"] = verified_files
    if provenance_path.is_file():
        evidence["provenance_sha256"] = sha256_of(provenance_path)
    if not problems and len(verified_files) == len(expected_files):
        root_document = {
            "schema": 1,
            "snapshot_id": snapshot_id,
            "sha256sums_sha256": evidence["sha256sums_sha256"],
            "files": [{"path": item["path"], "sha256": item["sha256"]} for item in verified_files],
        }
        evidence["snapshot_root_sha256"] = sha256_json(root_document)
    return problems, evidence


def verify_pack(pack_dir: Path, snapshot_id: str, sealed: dict[str, Any]) -> list[str]:
    return inspect_pack(pack_dir, snapshot_id, sealed)[0]


def materialize_train_manifest(lines: list[str], path: Path) -> dict[str, Any]:
    payload = ("\n".join(lines) + "\n").encode("utf-8")
    staging = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    staging.write_bytes(payload)
    os.replace(staging, path)
    return {"path": path.name, "sha256": hashlib.sha256(payload).hexdigest(), "rows": len(lines)}


def materialize_train_input(pack: dict[str, Any], out_dir: Path) -> dict[str, Any]:
    """Copy exactly the train rows and their WAVs into a closed, content-addressed input tree."""
    input_root = out_dir / TRAIN_INPUT_DIR
    input_root.mkdir(parents=True, exist_ok=True)
    train_rows = [row for row in pack["rows"] if row.get("split") == "train"]
    expected_rel = {TRAIN_MANIFEST_NAME, *(str(row["audio_path"]) for row in train_rows)}
    existing_rel = {
        path.relative_to(input_root).as_posix() for path in input_root.rglob("*") if path.is_file()
    }
    unexpected = sorted(existing_rel - expected_rel)
    if unexpected:
        raise ValueError(
            f"{TRAIN_INPUT_DIR} contains {len(unexpected)} stale/non-train file(s): {', '.join(unexpected[:5])}"
        )

    pack_root = Path(pack["pack_dir"])
    sealed_hashes = {item["path"]: item["sha256"] for item in pack["files"]}
    audio_files: list[dict[str, Any]] = []
    for row in train_rows:
        rel = PurePosixPath(row["audio_path"])
        source = pack_root.joinpath(*rel.parts)
        destination = input_root.joinpath(*rel.parts)
        destination.parent.mkdir(parents=True, exist_ok=True)
        staging = destination.with_name(f".{destination.name}.tmp-{os.getpid()}")
        shutil.copyfile(source, staging)
        os.replace(staging, destination)
        source_sha = sha256_of(source)
        copied_sha = sha256_of(destination)
        if copied_sha != source_sha or source_sha != sealed_hashes.get(rel.as_posix()):
            raise ValueError(f"copied train WAV {rel.as_posix()!r} does not match sealed source bytes")
        audio_files.append(
            {"path": rel.as_posix(), "sha256": copied_sha, "size_bytes": destination.stat().st_size}
        )
    manifest = materialize_train_manifest(pack["train_lines"], input_root / TRAIN_MANIFEST_NAME)
    manifest["path"] = f"{TRAIN_INPUT_DIR}/{TRAIN_MANIFEST_NAME}"
    manifest["audio_files"] = audio_files
    manifest["audio_files_count"] = len(audio_files)
    manifest["input_root_sha256"] = sha256_json(
        {
            "schema": 1,
            "manifest_sha256": manifest["sha256"],
            "audio_files": [{"path": item["path"], "sha256": item["sha256"]} for item in audio_files],
        }
    )
    return manifest


def verify_materialized_train_input(
    out_dir: Path, metadata: dict[str, Any], pack: dict[str, Any] | None = None
) -> list[str]:
    """Re-hash the trainer-visible tree and optionally compare it with the sealed pack's train rows."""
    problems: list[str] = []
    if metadata.get("path") != f"{TRAIN_INPUT_DIR}/{TRAIN_MANIFEST_NAME}":
        problems.append("train input manifest path is not the canonical isolated location")
    input_root = out_dir / TRAIN_INPUT_DIR
    if not input_root.is_dir():
        return [f"{TRAIN_INPUT_DIR} is missing"]
    manifest_path = input_root / TRAIN_MANIFEST_NAME
    if not manifest_path.is_file() or sha256_of(manifest_path) != metadata.get("sha256"):
        problems.append("train input manifest is missing or its hash changed")
    declared = metadata.get("audio_files")
    if not isinstance(declared, list) or not declared:
        problems.append("train input declares no audio files")
        declared = []
    if metadata.get("audio_files_count") != len(declared):
        problems.append("train input audio_files_count differs from its declared file list")
    verified: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, item in enumerate(declared):
        if not isinstance(item, dict):
            problems.append(f"train audio declaration {index} is not an object")
            continue
        rel = _safe_relative(item.get("path"), f"train audio {index} path", problems)
        if rel is None:
            continue
        rel_text = rel.as_posix()
        if rel_text in seen:
            problems.append(f"train audio {rel_text!r} is declared twice")
        seen.add(rel_text)
        path = _inside(input_root.resolve(strict=True), rel, "train audio", problems)
        if path is None:
            continue
        actual_hash = sha256_of(path)
        actual_size = path.stat().st_size
        if not is_sha256(item.get("sha256")) or actual_hash != item.get("sha256"):
            problems.append(f"train audio {rel_text!r} hash changed")
        if item.get("size_bytes") != actual_size:
            problems.append(f"train audio {rel_text!r} size changed")
        verified.append({"path": rel_text, "sha256": actual_hash, "size_bytes": actual_size})
    expected_files = {TRAIN_MANIFEST_NAME, *seen}
    actual_files = {
        path.relative_to(input_root).as_posix() for path in input_root.rglob("*") if path.is_file()
    }
    if actual_files != expected_files:
        problems.append("train input tree contains missing or extra files")
    root_sha = sha256_json(
        {
            "schema": 1,
            "manifest_sha256": metadata.get("sha256"),
            "audio_files": [{"path": item["path"], "sha256": item["sha256"]} for item in verified],
        }
    )
    if root_sha != metadata.get("input_root_sha256"):
        problems.append("train input root sha256 changed")
    if pack is not None:
        pack_files = {item["path"]: item for item in pack.get("files", [])}
        expected_audio = {
            row["audio_path"]: pack_files.get(row["audio_path"])
            for row in pack["rows"]
            if row.get("split") == "train"
        }
        actual_audio = {item["path"]: item for item in verified}
        if set(actual_audio) != set(expected_audio):
            problems.append("train input audio set differs from sealed train split")
        for rel_text, expected in expected_audio.items():
            if expected is None or actual_audio.get(rel_text, {}).get("sha256") != expected.get("sha256"):
                problems.append(f"train input {rel_text!r} differs from sealed train WAV")
    return problems


def unexpected_preexisting_run_files(out_dir: Path) -> list[str]:
    """A real trainer must start from a clean run, except for prepared input + run record."""
    allowed_top = {TRAIN_INPUT_DIR, RUN_RECORD_NAME}
    return sorted(
        path.relative_to(out_dir).as_posix()
        for path in out_dir.iterdir()
        if path.name not in allowed_top
    )


def verify_base_checkpoint(path_text: str, base_id: str, expected_sha256: str) -> tuple[list[str], dict[str, Any]]:
    problems: list[str] = []
    if not path_text:
        problems.append("no --base checkpoint; real training requires an exact champion checkpoint file")
    if not base_id:
        problems.append("no --base-id; a hash alone cannot prove registry/model family identity")
    if not is_sha256(expected_sha256):
        problems.append("no canonical --base-sha256; real training may not trust an unpinned base")
    if problems:
        return problems, {}
    try:
        path = Path(path_text).resolve(strict=True)
    except (FileNotFoundError, OSError, RuntimeError):
        return [f"base checkpoint does not exist: {path_text}"], {}
    if not path.is_file():
        return [f"base checkpoint is not one immutable file: {path}"], {}
    actual = sha256_of(path)
    if actual != expected_sha256:
        return [f"base checkpoint sha256 mismatch ({actual[:16]}… != {expected_sha256[:16]}…)"], {}
    return [], {"id": base_id, "path": str(path), "sha256": actual, "size_bytes": path.stat().st_size}


def parse_trainer_command(raw: str) -> list[str]:
    """Parse a command without a shell; a JSON argv array is the unambiguous portable form."""
    stripped = raw.strip()
    if stripped.startswith("["):
        value = json.loads(stripped)
        if not isinstance(value, list) or not value or any(not isinstance(item, str) or not item for item in value):
            raise ValueError("trainer JSON command must be a non-empty array of non-empty strings")
        return value
    tokens = shlex.split(raw, posix=os.name != "nt")
    if os.name == "nt":
        tokens = [
            token[1:-1] if len(token) >= 2 and token[0] == token[-1] and token[0] in {"'", '"'} else token
            for token in tokens
        ]
    if not tokens or any(not token for token in tokens):
        raise ValueError("trainer command is empty")
    return tokens


def _load_object(path: Path, label: str, problems: list[str]) -> dict[str, Any] | None:
    if not path.is_file():
        problems.append(f"trainer did not produce {label}")
        return None
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError) as error:
        problems.append(f"{label} is unreadable: {error}")
        return None
    if not isinstance(value, dict):
        problems.append(f"{label} is not a JSON object")
        return None
    return value


def verify_trainer_outputs(
    out_dir: Path, contract: dict[str, Any], contract_sha256: str
) -> tuple[list[str], dict[str, Any]]:
    """Verify consumption evidence and every trainer-declared model artifact."""
    problems: list[str] = []
    result: dict[str, Any] = {}
    try:
        root = out_dir.resolve(strict=True)
    except (FileNotFoundError, OSError, RuntimeError):
        return [f"trainer output directory disappeared: {out_dir}"], result
    evidence_path = root / TRAINER_EVIDENCE_NAME
    artifact_manifest_path = root / ARTIFACT_MANIFEST_NAME
    evidence = _load_object(evidence_path, TRAINER_EVIDENCE_NAME, problems)
    artifact_manifest = _load_object(artifact_manifest_path, ARTIFACT_MANIFEST_NAME, problems)
    expected = {
        "snapshot_id": contract["snapshot"]["id"],
        "snapshot_root_sha256": contract["snapshot"]["root_sha256"],
        "training_contract_sha256": contract_sha256,
        "train_manifest_sha256": contract["train_manifest"]["sha256"],
        "train_rows_consumed": contract["train_manifest"]["rows"],
        "train_audio_files_consumed": contract["train_manifest"]["audio_files_count"],
        "train_input_root_sha256": contract["train_manifest"]["input_root_sha256"],
        "base_id": contract["base"]["id"],
        "base_sha256": contract["base"]["sha256"],
    }
    if evidence is not None:
        if evidence.get("schema") != 1:
            problems.append(f"{TRAINER_EVIDENCE_NAME} schema is not 1")
        if evidence.get("status") != "completed":
            problems.append(f"{TRAINER_EVIDENCE_NAME} status is not 'completed'")
        for key, expected_value in expected.items():
            if evidence.get(key) != expected_value:
                problems.append(f"{TRAINER_EVIDENCE_NAME} does not attest exact {key}")
        if not isinstance(evidence.get("trainer_identity"), str) or not evidence["trainer_identity"]:
            problems.append(f"{TRAINER_EVIDENCE_NAME} has no trainer_identity")
        for key in ("recipe_sha256", "environment_sha256"):
            if not is_sha256(evidence.get(key)):
                problems.append(f"{TRAINER_EVIDENCE_NAME} has no canonical {key}")
        seed = evidence.get("seed")
        if not isinstance(seed, int) or isinstance(seed, bool) or seed < 0:
            problems.append(f"{TRAINER_EVIDENCE_NAME} has invalid deterministic seed {seed!r}")

    artifacts: list[dict[str, Any]] = []
    role_map: dict[str, dict[str, Any]] = {}
    if artifact_manifest is not None:
        if artifact_manifest.get("schema") != 1:
            problems.append(f"{ARTIFACT_MANIFEST_NAME} schema is not 1")
        for key, expected_value in expected.items():
            if key in {"train_rows_consumed", "train_audio_files_consumed"}:
                continue
            if artifact_manifest.get(key) != expected_value:
                problems.append(f"{ARTIFACT_MANIFEST_NAME} does not bind exact {key}")
        declared = artifact_manifest.get("artifacts")
        if not isinstance(declared, list) or not declared:
            problems.append(f"{ARTIFACT_MANIFEST_NAME} declares no model artifacts")
            declared = []
        seen_paths: set[str] = set()
        for index, item in enumerate(declared):
            if not isinstance(item, dict):
                problems.append(f"{ARTIFACT_MANIFEST_NAME} artifact {index} is not an object")
                continue
            role = item.get("role")
            if role not in {"adapter", "tokenizer", "adapter_config", "support"}:
                problems.append(f"artifact {index} has unknown role {role!r}")
            elif role != "support" and role in role_map:
                problems.append(f"artifact role {role!r} is declared more than once")
            rel = _safe_relative(item.get("path"), f"artifact {index} path", problems)
            digest = item.get("sha256")
            size = item.get("size_bytes")
            if rel is None:
                continue
            rel_text = rel.as_posix()
            if rel_text in CONTROL_FILES or rel.parts[0] == TRAIN_INPUT_DIR:
                problems.append(f"artifact {rel_text!r} is an evidence file, not a model artifact")
            if rel_text in seen_paths:
                problems.append(f"artifact {rel_text!r} is declared twice")
            seen_paths.add(rel_text)
            if not is_sha256(digest):
                problems.append(f"artifact {rel_text!r} has no canonical sha256")
            if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
                problems.append(f"artifact {rel_text!r} has invalid size_bytes {size!r}")
            actual_path = _inside(root, rel, "artifact", problems)
            if actual_path is None:
                continue
            actual_hash = sha256_of(actual_path)
            actual_size = actual_path.stat().st_size
            if is_sha256(digest) and actual_hash != digest:
                problems.append(f"artifact {rel_text!r} hash does not match its manifest")
            if isinstance(size, int) and not isinstance(size, bool) and actual_size != size:
                problems.append(f"artifact {rel_text!r} size does not match its manifest")
            verified = {"role": role, "path": rel_text, "sha256": actual_hash, "size_bytes": actual_size}
            artifacts.append(verified)
            if role in {"adapter", "tokenizer", "adapter_config"}:
                role_map[role] = verified
        for required in ("adapter", "tokenizer", "adapter_config"):
            if required not in role_map:
                problems.append(f"trained artifact set is missing required {required!r} role")
        if "adapter" in role_map and "adapter_config" in role_map:
            adapter_rel = PurePosixPath(role_map["adapter"]["path"])
            config_rel = PurePosixPath(role_map["adapter_config"]["path"])
            if adapter_rel.name != "adapter_model.safetensors":
                problems.append("adapter role must be a file named adapter_model.safetensors")
            if config_rel.name != "adapter_config.json":
                problems.append("adapter_config role must be a file named adapter_config.json")
            if adapter_rel.parent != config_rel.parent:
                problems.append("adapter_model.safetensors and adapter_config.json must share one PEFT directory")

    if evidence_path.is_file():
        result["trainer_evidence_sha256"] = sha256_of(evidence_path)
    if artifact_manifest_path.is_file():
        result["artifact_manifest_sha256"] = sha256_of(artifact_manifest_path)
    result.update(evidence=evidence, artifacts=artifacts, roles=role_map)
    return problems, result


def build_deployment_manifest(
    contract: dict[str, Any],
    contract_sha256: str,
    outputs: dict[str, Any],
    trainer_command: str,
) -> dict[str, Any]:
    """Build the registry/server handoff with a canonical self-hash."""
    evidence = outputs["evidence"]
    roles = outputs["roles"]
    training_run_id = sha256_json(
        {
            "training_contract_sha256": contract_sha256,
            "trainer_evidence_sha256": outputs["trainer_evidence_sha256"],
            "artifact_manifest_sha256": outputs["artifact_manifest_sha256"],
        }
    )
    model_id = f"omniasr-7b-challenger-{training_run_id[:24]}"

    def component(role: str) -> dict[str, Any]:
        item = roles[role]
        return {"path": item["path"], "sha256": item["sha256"], "size_bytes": item["size_bytes"]}

    manifest: dict[str, Any] = {
        "schema": 1,
        "model_id": model_id,
        "family": "omniasr-7b",
        "model_card": contract["model_card"],
        "language": "ckb_Arab",
        "runtime": {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288},
        "base_checkpoint": {
            "id": contract["base"]["id"],
            "path": contract["base"]["path"],
            "sha256": contract["base"]["sha256"],
            "size_bytes": contract["base"]["size_bytes"],
        },
        "adapter": component("adapter"),
        "tokenizer": component("tokenizer"),
        "adapter_config": component("adapter_config"),
        "server_compatibility": {"protocol": "cortex-omniasr-adapter", "version": 1},
        "provenance": {
            "kind": "flywheel",
            "snapshot": {
                "id": contract["snapshot"]["id"],
                "root_sha256": contract["snapshot"]["root_sha256"],
            },
            "training_run_id": training_run_id,
            "trainer": {
                "command": trainer_command,
                "identity": evidence["trainer_identity"],
                "recipe_sha256": evidence["recipe_sha256"],
                "environment_sha256": evidence["environment_sha256"],
                "seed": evidence["seed"],
            },
            "source_evidence": {
                "training_contract_sha256": contract_sha256,
                "trainer_evidence_sha256": outputs["trainer_evidence_sha256"],
                "artifact_manifest_sha256": outputs["artifact_manifest_sha256"],
            },
        },
    }
    manifest["manifestSha256"] = sha256_json(manifest)
    return manifest


def verify_deployment_manifest(path: Path, out_dir: Path) -> list[str]:
    """Recompute self-hash and serving-component hashes."""
    problems: list[str] = []
    raw_bytes = path.read_bytes() if path.is_file() else b""
    value = _load_object(path, DEPLOYMENT_MANIFEST_NAME, problems)
    if value is None:
        return problems
    stated = value.pop("manifestSha256", None)
    actual = sha256_json(value)
    value["manifestSha256"] = stated
    if b"\r" in raw_bytes or raw_bytes != canonical_json_bytes(value) + b"\n":
        problems.append(
            f"{DEPLOYMENT_MANIFEST_NAME} file bytes are not canonical sorted UTF-8 JSON plus one LF"
        )
    if stated != actual:
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} manifestSha256 does not match canonical JSON excluding self-hash")
    if value.get("schema") != 1 or value.get("family") != "omniasr-7b":
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has unsupported schema/family")
    if value.get("model_card") != OMNIASR_MODEL_CARD:
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} model_card is not the pinned OmniASR-7B card")
    if value.get("language") != "ckb_Arab":
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} language is not 'ckb_Arab'")
    if value.get("runtime") != {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288}:
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} runtime contract does not match OmniASR-7B")
    if not isinstance(value.get("model_id"), str) or not value["model_id"]:
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no model_id")
    provenance = value.get("provenance")
    if not isinstance(provenance, dict) or provenance.get("kind") != "flywheel":
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} is not tagged flywheel provenance")
        provenance = {}
    if not is_sha256(provenance.get("training_run_id")):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no training_run_id")
    snapshot = provenance.get("snapshot")
    if (
        not isinstance(snapshot, dict)
        or not is_sha256(snapshot.get("id"))
        or not is_sha256(snapshot.get("root_sha256"))
    ):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no complete snapshot provenance")
    trainer = provenance.get("trainer")
    if not isinstance(trainer, dict):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no trainer provenance")
    elif (
        not isinstance(trainer.get("command"), str)
        or not trainer["command"]
        or not isinstance(trainer.get("identity"), str)
        or not trainer["identity"]
        or not is_sha256(trainer.get("recipe_sha256"))
        or not is_sha256(trainer.get("environment_sha256"))
        or not isinstance(trainer.get("seed"), int)
        or isinstance(trainer.get("seed"), bool)
        or trainer["seed"] < 0
    ):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} trainer provenance is incomplete")
    source_evidence = provenance.get("source_evidence")
    evidence_keys = {"training_contract_sha256", "trainer_evidence_sha256", "artifact_manifest_sha256"}
    if (
        not isinstance(source_evidence, dict)
        or set(source_evidence) != evidence_keys
        or any(not is_sha256(source_evidence.get(key)) for key in evidence_keys)
    ):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has incomplete source evidence")
    else:
        expected_run_id = sha256_json(source_evidence)
        if provenance.get("training_run_id") != expected_run_id:
            problems.append(f"{DEPLOYMENT_MANIFEST_NAME} training_run_id does not derive from source evidence")
        if value.get("model_id") != f"omniasr-7b-challenger-{expected_run_id[:24]}":
            problems.append(f"{DEPLOYMENT_MANIFEST_NAME} model_id does not derive from training_run_id")
    if value.get("server_compatibility") != {"protocol": "cortex-omniasr-adapter", "version": 1}:
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} server compatibility is unsupported")
    root = out_dir.resolve(strict=True)
    adapter_value = value.get("adapter")
    config_value = value.get("adapter_config")
    if isinstance(adapter_value, dict) and isinstance(config_value, dict):
        adapter_rel = PurePosixPath(str(adapter_value.get("path") or ""))
        config_rel = PurePosixPath(str(config_value.get("path") or ""))
        if (
            adapter_rel.name != "adapter_model.safetensors"
            or config_rel.name != "adapter_config.json"
            or adapter_rel.parent != config_rel.parent
        ):
            problems.append(f"{DEPLOYMENT_MANIFEST_NAME} does not identify one loadable PEFT adapter directory")
    base = value.get("base_checkpoint")
    if not isinstance(base, dict):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no base_checkpoint component")
    else:
        base_problems, verified_base = verify_base_checkpoint(
            str(base.get("path") or ""), str(base.get("id") or ""), str(base.get("sha256") or "")
        )
        problems.extend(f"deployment base_checkpoint: {problem}" for problem in base_problems)
        if verified_base and base.get("size_bytes") != verified_base["size_bytes"]:
            problems.append("deployment base_checkpoint size differs from manifest")
    for key in ("adapter", "tokenizer"):
        component_value = value.get(key)
        if not isinstance(component_value, dict):
            problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no {key} component")
            continue
        rel = _safe_relative(component_value.get("path"), f"deployment {key} path", problems)
        if rel is None:
            continue
        actual_path = _inside(root, rel, f"deployment {key}", problems)
        if not is_sha256(component_value.get("sha256")):
            problems.append(f"deployment {key} has no canonical sha256")
        if actual_path is not None:
            if sha256_of(actual_path) != component_value.get("sha256"):
                problems.append(f"deployment {key} file does not match declared sha256")
            if component_value.get("size_bytes") != actual_path.stat().st_size:
                problems.append(f"deployment {key} file does not match declared size")
    component_value = value.get("adapter_config")
    if not isinstance(component_value, dict):
        problems.append(f"{DEPLOYMENT_MANIFEST_NAME} has no required adapter_config component")
    else:
        rel = _safe_relative(component_value.get("path"), "deployment adapter_config path", problems)
        if rel is not None:
            actual_path = _inside(root, rel, "deployment adapter_config", problems)
            if not is_sha256(component_value.get("sha256")):
                problems.append("deployment adapter_config has no canonical sha256")
            if actual_path is not None:
                if sha256_of(actual_path) != component_value.get("sha256"):
                    problems.append("deployment adapter_config file does not match declared sha256")
                if component_value.get("size_bytes") != actual_path.stat().st_size:
                    problems.append("deployment adapter_config file does not match declared size")
    return problems


def _base_record(
    args: argparse.Namespace, sealed: dict[str, Any], pack: dict[str, Any], train_manifest: dict[str, Any]
) -> dict[str, Any]:
    return {
        "schema": 2,
        "status": "prepared",
        "snapshot_id": args.snapshot,
        "snapshot_sealed_at": sealed.get("created_at"),
        "snapshot_selection": sealed.get("config"),
        "pack_dir": pack["pack_dir"],
        "manifest_sha256": pack["manifest_sha256"],
        "sha256sums_sha256": pack["sha256sums_sha256"],
        "snapshot_root_sha256": pack["snapshot_root_sha256"],
        "rows_total": pack["rows_total"],
        "rows_by_split": pack["rows_by_split"],
        "train_manifest": train_manifest,
        "base_id": args.base_id or None,
        "model_card": args.model_card or None,
        "base_checkpoint": args.base or None,
        "base_sha256": args.base_sha256 or None,
        "trainer_command": args.trainer or None,
        "family_lock": "LoRA on the pinned OmniASR-7B champion family only",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Train a challenger LoRA from one sealed snapshot")
    parser.add_argument("--snapshot", required=True, help="dataset snapshot id (manifest sha256)")
    parser.add_argument("--pack", required=True, type=Path, help="exported snapshot root")
    parser.add_argument("--out", type=Path, help="run directory")
    parser.add_argument(
        "--trainer",
        default=os.environ.get("CORTEX_CHALLENGER_TRAINER", ""),
        help="external trainer; receives --manifest, --out, --base, and --contract",
    )
    parser.add_argument("--base", default=os.environ.get("CORTEX_CHALLENGER_BASE", ""))
    parser.add_argument("--base-id", default=os.environ.get("CORTEX_CHALLENGER_BASE_ID", ""))
    parser.add_argument("--base-sha256", default=os.environ.get("CORTEX_CHALLENGER_BASE_SHA256", ""))
    parser.add_argument("--model-card", default=os.environ.get("CORTEX_CHALLENGER_MODEL_CARD", ""))
    parser.add_argument("--dry-run", action="store_true", help="prepare evidence; never invoke trainer")
    args = parser.parse_args()

    sealed = sealed_snapshot(args.snapshot)
    if sealed is None:
        print(f"CHALLENGER: REFUSED — snapshot {args.snapshot[:16]}… was never sealed", flush=True)
        return 2
    pack_problems, pack = inspect_pack(args.pack, args.snapshot, sealed)
    if pack_problems:
        print("CHALLENGER: REFUSED — snapshot root failed integrity", flush=True)
        for problem in pack_problems:
            print(f"  - {problem}", flush=True)
        return 2

    out_dir = (args.out or (RUNS_DIR / f"challenger_{args.snapshot[:12]}")).resolve()
    pack_root = Path(pack["pack_dir"])
    try:
        out_dir.relative_to(pack_root)
    except ValueError:
        pass
    else:
        print("CHALLENGER: REFUSED — --out may not be inside immutable snapshot root", flush=True)
        return 2
    out_dir.mkdir(parents=True, exist_ok=True)
    try:
        train_manifest = materialize_train_input(pack, out_dir)
    except (OSError, ValueError) as error:
        print(f"CHALLENGER: REFUSED — could not materialize train-only input: {error}", flush=True)
        return 2
    record = _base_record(args, sealed, pack, train_manifest)

    print(f"CHALLENGER: snapshot root {pack['snapshot_root_sha256'][:16]}… verified", flush=True)
    if args.dry_run or not args.trainer:
        record["note"] = "PREPARED and integrity-checked, not trained; an exit code is not model evidence."
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        print(f"CHALLENGER: PREPARED (not trained) — {out_dir / RUN_RECORD_NAME}", flush=True)
        return 3

    stale_run_files = unexpected_preexisting_run_files(out_dir)
    if stale_run_files:
        record.update(
            status="failed",
            failure_stage="run-directory-preflight",
            problems=[f"run directory is not clean: {', '.join(stale_run_files[:5])}"],
        )
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        print("CHALLENGER: REFUSED — real training requires a clean run directory", flush=True)
        return 2

    base_problems, base = verify_base_checkpoint(args.base, args.base_id, args.base_sha256)
    if args.model_card != OMNIASR_MODEL_CARD:
        base_problems.append(
            f"--model-card must be the pinned OmniASR-7B card {OMNIASR_MODEL_CARD!r}"
        )
    if base_problems:
        record.update(status="failed", failure_stage="base-preflight", problems=base_problems)
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        print("CHALLENGER: REFUSED — exact base identity is not proven", flush=True)
        return 2

    contract = {
        "schema": 1,
        "snapshot": {
            "id": args.snapshot,
            "root_sha256": pack["snapshot_root_sha256"],
        },
        "train_manifest": train_manifest,
        "base": base,
        "model_card": args.model_card,
        "required_outputs": [TRAINER_EVIDENCE_NAME, ARTIFACT_MANIFEST_NAME],
    }
    atomic_write_json(out_dir / CONTRACT_NAME, contract)
    contract_sha = sha256_of(out_dir / CONTRACT_NAME)
    record.update(
        base_id=base["id"],
        base_checkpoint=base["path"],
        base_sha256=base["sha256"],
        training_contract={"path": CONTRACT_NAME, "sha256": contract_sha},
        note="trainer invocation pending; this is not a trained result",
    )
    atomic_write_json(out_dir / RUN_RECORD_NAME, record)
    try:
        trainer_argv = parse_trainer_command(args.trainer)
    except (json.JSONDecodeError, ValueError) as error:
        record.update(status="failed", failure_stage="trainer-command", problems=[str(error)])
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        return 2
    if not trainer_argv:
        record.update(status="failed", failure_stage="trainer-command", problems=["trainer command is empty"])
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        return 2
    command = [
        *trainer_argv,
        "--manifest",
        str(out_dir / train_manifest["path"]),
        "--out",
        str(out_dir),
        "--base",
        base["path"],
        "--contract",
        str(out_dir / CONTRACT_NAME),
    ]
    record["trainer_argv"] = command
    print(f"  invoking trainer with {train_manifest['rows']} train-only row(s)", flush=True)
    try:
        completed = subprocess.run(command, cwd=REPO_ROOT, check=False)
        exit_code: int | None = completed.returncode
        launch_problem = ""
    except OSError as error:
        exit_code = None
        launch_problem = f"trainer could not start: {error}"
    record["trainer_exit_code"] = exit_code

    postflight: list[str] = []
    if launch_problem:
        postflight.append(launch_problem)
    if exit_code != 0:
        postflight.append(f"trainer exited {exit_code!r}, not 0")
    pack_after_problems, pack_after = inspect_pack(Path(pack["pack_dir"]), args.snapshot, sealed)
    postflight.extend(f"snapshot postflight: {problem}" for problem in pack_after_problems)
    if pack_after.get("snapshot_root_sha256") != pack["snapshot_root_sha256"]:
        postflight.append("snapshot root changed during training")
    materialized_manifest_path = out_dir / train_manifest["path"]
    if not materialized_manifest_path.is_file() or sha256_of(materialized_manifest_path) != train_manifest["sha256"]:
        postflight.append("trainer changed the materialized train-only manifest")
    postflight.extend(
        f"train input postflight: {problem}"
        for problem in verify_materialized_train_input(out_dir, train_manifest, pack)
    )
    contract_path = out_dir / CONTRACT_NAME
    if not contract_path.is_file() or sha256_of(contract_path) != contract_sha:
        postflight.append("trainer changed its immutable training contract")
    output_problems, outputs = verify_trainer_outputs(out_dir, contract, contract_sha)
    postflight.extend(output_problems)
    if postflight:
        record.update(status="failed", failure_stage="trainer-postflight", problems=postflight)
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        print("CHALLENGER: FAILED — exit 0 alone is not training proof", flush=True)
        for problem in postflight:
            print(f"  - {problem}", flush=True)
        return 1

    deployment = build_deployment_manifest(contract, contract_sha, outputs, args.trainer)
    atomic_write_canonical_json(out_dir / DEPLOYMENT_MANIFEST_NAME, deployment)
    deployment_problems = verify_deployment_manifest(out_dir / DEPLOYMENT_MANIFEST_NAME, out_dir)
    if deployment_problems:
        record.update(status="failed", failure_stage="deployment-manifest", problems=deployment_problems)
        atomic_write_json(out_dir / RUN_RECORD_NAME, record)
        return 1
    deployment_file_sha = sha256_of(out_dir / DEPLOYMENT_MANIFEST_NAME)
    record.update(
        status="trained",
        training_run_id=deployment["provenance"]["training_run_id"],
        model_id=deployment["model_id"],
        trainer_evidence={"path": TRAINER_EVIDENCE_NAME, "sha256": outputs["trainer_evidence_sha256"]},
        artifact_manifest={"path": ARTIFACT_MANIFEST_NAME, "sha256": outputs["artifact_manifest_sha256"]},
        artifact_manifest_sha256=outputs["artifact_manifest_sha256"],
        deployment_manifest={
            "path": DEPLOYMENT_MANIFEST_NAME,
            "sha256": deployment_file_sha,
            "manifestSha256": deployment["manifestSha256"],
        },
        artifacts=outputs["artifacts"],
        note="trained only after verified consumption, artifacts, and registry/server deployment manifest",
    )
    atomic_write_json(out_dir / RUN_RECORD_NAME, record)
    print(f"CHALLENGER: TRAINED + VERIFIED — {out_dir / RUN_RECORD_NAME}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
