"""Shared exact-focus contract for the controlled paid-review pilot.

The tracked app-root JSON is the single machine-readable authority.  Its digest input is exactly
``("\\n".join(sorted(unique_ids)) + "\\n").encode("utf-8")``.  This module is intentionally
dependency-free so activation and every production gate can consume the same contract.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

CONTRACT_FILE = "controlled_pilot_focus.json"
VOICE_FOCUS_FILE = "voice_focus.json"
CONTRACT_PATH = Path(__file__).resolve().parents[1] / CONTRACT_FILE
CANONICALIZATION = "utf8_sorted_unique_ids_lf_join_final_lf_v1"


class PilotFocusError(RuntimeError):
    """The controlled pilot cannot prove its exact focus."""


@dataclass(frozen=True)
class PilotFocusContract:
    segment_id_count: int
    sorted_unique_segment_ids_sha256: str


@dataclass(frozen=True)
class PilotFocusEvidence:
    segment_id_count: int
    sorted_unique_segment_ids_sha256: str


def _reject_duplicate_object_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON object key")
        value[key] = item
    return value


def _strict_json_loads(raw: str) -> object:
    def reject_constant(value: str) -> object:
        raise ValueError(f"non-finite JSON number: {value}")

    return json.loads(raw, object_pairs_hook=_reject_duplicate_object_keys, parse_constant=reject_constant)


def parse_pilot_focus_contract(raw: str, source: str = CONTRACT_FILE) -> PilotFocusContract:
    try:
        value = _strict_json_loads(raw)
    except (json.JSONDecodeError, ValueError) as error:
        raise PilotFocusError(f"{source} is invalid JSON: {error}") from error
    expected_fields = {
        "schema_version",
        "segment_id_count",
        "sorted_unique_segment_ids_sha256",
        "canonicalization",
    }
    if not isinstance(value, dict) or set(value) != expected_fields:
        raise PilotFocusError(f"{source} fields do not exactly match the controlled-pilot focus contract")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise PilotFocusError(f"{source} schema_version must be integer 1")
    count = value["segment_id_count"]
    if type(count) is not int or count <= 0:
        raise PilotFocusError(f"{source} segment_id_count must be a positive integer")
    digest = value["sorted_unique_segment_ids_sha256"]
    if (
        not isinstance(digest, str)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        raise PilotFocusError(f"{source} digest must be a canonical lowercase SHA-256")
    if value["canonicalization"] != CANONICALIZATION:
        raise PilotFocusError(f"{source} names an unsupported ID canonicalization")
    return PilotFocusContract(count, digest)


def load_pilot_focus_contract(path: Path = CONTRACT_PATH) -> PilotFocusContract:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as error:
        raise PilotFocusError(f"{path} is unreadable: {error}") from error
    return parse_pilot_focus_contract(raw, str(path))


def focus_evidence(segment_ids: Iterable[str]) -> PilotFocusEvidence:
    raw_ids = list(segment_ids)
    for segment_id in raw_ids:
        if not isinstance(segment_id, str):
            raise PilotFocusError("voice_focus.json contains a non-string segment id")
        if not segment_id or "\n" in segment_id or "\r" in segment_id:
            raise PilotFocusError("voice_focus.json contains an empty or newline-bearing segment id")
    unique_ids = sorted(set(raw_ids))
    payload = ("\n".join(unique_ids) + "\n").encode("utf-8")
    return PilotFocusEvidence(len(unique_ids), hashlib.sha256(payload).hexdigest())


def contract_for_ids(segment_ids: Iterable[str]) -> PilotFocusContract:
    """Build an injected contract for deterministic unit tests; production never calls this."""
    evidence = focus_evidence(segment_ids)
    return PilotFocusContract(evidence.segment_id_count, evidence.sorted_unique_segment_ids_sha256)


def _load_voice_focus_ids(data_dir: Path) -> set[str]:
    path = data_dir / VOICE_FOCUS_FILE
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError as error:
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} is required while controlled review is active") from error
    except OSError as error:
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} is unreadable: {error}") from error
    try:
        value = _strict_json_loads(raw)
    except (json.JSONDecodeError, ValueError) as error:
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} is invalid JSON: {error}") from error
    items = value.get("segment_ids") if isinstance(value, dict) else None
    ids = {item for item in items if isinstance(item, str)} if isinstance(items, list) else set()
    if not ids:
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} names no segment ids")
    return ids


def verify_controlled_pilot_focus(
    data_dir: Path,
    contract: PilotFocusContract | None = None,
) -> PilotFocusEvidence:
    expected = contract or load_pilot_focus_contract()
    actual = focus_evidence(_load_voice_focus_ids(data_dir))
    if actual.segment_id_count != expected.segment_id_count:
        raise PilotFocusError(
            "controlled-pilot voice focus has "
            f"{actual.segment_id_count} unique ids; expected exactly {expected.segment_id_count}"
        )
    if actual.sorted_unique_segment_ids_sha256 != expected.sorted_unique_segment_ids_sha256:
        raise PilotFocusError(
            "controlled-pilot voice focus digest mismatch: "
            f"found {actual.sorted_unique_segment_ids_sha256}, "
            f"expected {expected.sorted_unique_segment_ids_sha256}"
        )
    return actual
