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
CANONICAL_IDENTITY_KIND = "audio_content_hash+source_span"
PLAYBACK_GUARD_VERSION = "content-hash-raw-counter-v3"
MAX_SOURCE_SPAN_DURATION_DELTA_MS = 1


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


def is_canonical_content_hash(value: object) -> bool:
    """Return whether *value* has the canonical lowercase 64-hex BLAKE3 content-hash shape."""
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def canonical_source_span(alignment_json: object) -> tuple[tuple[int, int] | None, str | None]:
    """Extract the exact source window from strict server-owned alignment JSON."""
    if not isinstance(alignment_json, str):
        return None, "server-owned alignment_json is not text"
    try:
        parsed = _strict_json_loads(alignment_json)
    except (json.JSONDecodeError, ValueError) as error:
        return None, f"server-owned alignment_json is malformed: {error}"
    if not isinstance(parsed, dict):
        return None, "server-owned alignment_json is not an object"
    start = parsed.get("source_start_ms")
    end = parsed.get("source_end_ms")
    if type(start) is not int or type(end) is not int:
        return None, "server-owned source span coordinates are not exact integers"
    if start < 0 or end <= start:
        return None, f"server-owned source span ({start}, {end}) is not a non-empty forward range"
    return (start, end), None


def source_span_duration_issue(
    duration_ms: object,
    source_span: tuple[int, int],
    *,
    subject: str = "server-owned duration",
) -> str | None:
    """Reject payment/playback denominators that are not the decoded clip's source-window length.

    The extractor's integer millisecond endpoints may differ from the decoded duration by one
    millisecond because of endpoint rounding. Anything larger is not the same payable clip.
    """
    if type(duration_ms) is not int or duration_ms <= 0:
        return f"{subject} {duration_ms!r}ms is not a positive integer"
    source_duration_ms = source_span[1] - source_span[0]
    difference_ms = abs(duration_ms - source_duration_ms)
    if difference_ms > MAX_SOURCE_SPAN_DURATION_DELTA_MS:
        return (
            f"{subject} {duration_ms}ms differs from exact source span length "
            f"{source_duration_ms}ms by {difference_ms}ms; maximum endpoint-rounding "
            f"tolerance is {MAX_SOURCE_SPAN_DURATION_DELTA_MS}ms"
        )
    return None


def canonical_audio_work_id(
    audio_content_hash: object,
    alignment_json: object,
) -> tuple[str | None, str]:
    """Derive the sole paid-audio identity; segment-id fallback is intentionally impossible."""
    if not is_canonical_content_hash(audio_content_hash):
        return None, "audio_content_hash is not canonical lowercase 64-hex"
    if not isinstance(alignment_json, str):
        return None, "alignment_json is not text"
    try:
        alignment = _strict_json_loads(alignment_json)
    except (json.JSONDecodeError, ValueError) as error:
        return None, f"alignment_json is invalid: {error}"
    if not isinstance(alignment, dict):
        return None, "alignment_json is not an object"
    start = alignment.get("source_start_ms")
    end = alignment.get("source_end_ms")
    # JSON booleans become bool, which is an int subclass. They are never valid coordinates.
    if type(start) is not int or type(end) is not int or start < 0 or end <= start:
        return None, "alignment_json lacks an exact non-empty source span"
    return f"audio-segment-v1:{audio_content_hash}:{start}:{end}", ""


def canonical_reviewer_work_id(
    reviewer: object,
    audio_content_hash: object,
    alignment_json: object,
) -> tuple[str | None, str]:
    """Derive the exact reviewer/audio natural identity used by compensation policy v2."""
    if not isinstance(reviewer, str) or not reviewer.strip():
        return None, "reviewer is empty"
    reviewer_key = reviewer.strip().lower()
    audio_work_id, reason = canonical_audio_work_id(audio_content_hash, alignment_json)
    if audio_work_id is None:
        return None, reason
    reviewer_size = len(reviewer_key.encode("utf-8"))
    return f"reviewer-work-v1:{reviewer_size}:{reviewer_key}:{audio_work_id}", ""


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


def load_voice_focus_ids(data_dir: Path) -> set[str]:
    """Load the exact active focus membership, rejecting ambiguous list shapes.

    The digest proves the set as a whole; certification also needs the actual IDs so every paid
    event can be proven to belong to that set.  Rejecting non-strings and duplicates here prevents
    a caller from validating a filtered subset that was never the file's literal membership.
    """
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
    if not isinstance(items, list) or not items:
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} names no segment ids")
    if not all(isinstance(item, str) and item and "\n" not in item and "\r" not in item for item in items):
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} contains an invalid segment id")
    if len(items) != len(set(items)):
        raise PilotFocusError(f"{VOICE_FOCUS_FILE} contains duplicate segment ids")
    return set(items)


def verify_controlled_pilot_focus(
    data_dir: Path,
    contract: PilotFocusContract | None = None,
) -> PilotFocusEvidence:
    expected = contract or load_pilot_focus_contract()
    actual = focus_evidence(load_voice_focus_ids(data_dir))
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
