#!/usr/bin/env python3
"""Canonical owner-proof input contract parsing and structural validation."""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Mapping

from owner_proof_platform import ProofInputError


MAX_JSON_BYTES = 2 * 1024 * 1024
SOURCE_ROLES = (
    "real-media-mp4",
    "real-media-mov",
    "real-media-flac",
    "long-audiobook-mp3",
    "scale-database-authority",
    "campaign-database-authority",
)
MEDIA_ROLES = ("real-media-mp4", "real-media-mov", "real-media-flac")
DATABASE_ROLES = ("scale-database-authority", "campaign-database-authority")
LOWER_SHA256 = re.compile(r"[0-9a-f]{64}")
FULL_GIT_SHA = re.compile(r"[0-9a-f]{40}")
TOOLCHAIN_HASH_FIELDS = (
    "cargoBinarySha256",
    "rustcBinarySha256",
    "gitBinarySha256",
    "cargoConfigSha256",
    "clBinarySha256",
    "linkBinarySha256",
    "libBinarySha256",
    "rcBinarySha256",
    "mtBinarySha256",
    "msvcTreeSha256",
    "windowsSdkTreeSha256",
    "rustRuntimeTreeSha256",
    "gitRuntimeTreeSha256",
)
HELPER_BUILD_TOOLCHAIN_FIELDS = (
    *TOOLCHAIN_HASH_FIELDS,
    "cargoCommitHash",
    "rustcCommitHash",
    "gitVersion",
    "msvcToolsVersion",
    "windowsSdkVersion",
)


@dataclass(frozen=True)
class SourcePaths:
    media_mp4: Path
    media_mov: Path
    media_flac: Path
    audiobook_mp3: Path
    scale_db: Path
    campaign_db: Path
    migration_helper: Path | None = None

    def by_role(self) -> dict[str, Path]:
        return {
            "real-media-mp4": self.media_mp4,
            "real-media-mov": self.media_mov,
            "real-media-flac": self.media_flac,
            "long-audiobook-mp3": self.audiobook_mp3,
            "scale-database-authority": self.scale_db,
            "campaign-database-authority": self.campaign_db,
        }


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ProofInputError(f"JSON contains duplicate key {key!r}")
        result[key] = value
    return result


def _reject_nonfinite_constant(value: str) -> None:
    raise ProofInputError(f"strict JSON cannot contain non-finite constant {value}")


def parse_json_bytes(raw: bytes, *, context: str = "proof JSON") -> Any:
    """Decode bounded strict JSON and normalize all parser/serializer failures to one typed boundary."""
    if len(raw) > MAX_JSON_BYTES:
        raise ProofInputError(f"{context} exceeds the bounded input size")
    try:
        text = raw.decode("utf-8", errors="strict")
        value = json.loads(
            text,
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=_reject_nonfinite_constant,
        )
        # Validate that every decoded string/value can cross the canonical UTF-8
        # boundary. This rejects escaped lone surrogates and excessive recursion
        # even when canonical byte equality is not required by the caller.
        canonical_json_bytes(value)
        return value
    except ProofInputError:
        raise
    except (UnicodeError, ValueError, TypeError, RecursionError, OverflowError) as error:
        raise ProofInputError(f"{context} is not bounded strict UTF-8 JSON") from error


def load_json(path: Path, *, canonical: bool = False) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ProofInputError("proof JSON cannot be read") from error
    value = parse_json_bytes(raw)
    if not isinstance(value, dict):
        raise ProofInputError("proof JSON must be an object")
    if canonical and raw != canonical_json_bytes(value):
        raise ProofInputError("proof JSON is not in canonical byte form")
    return value


def canonical_json_bytes(value: Any) -> bytes:
    try:
        text = json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
        return (text + "\n").encode("utf-8", errors="strict")
    except (UnicodeError, ValueError, TypeError, RecursionError, OverflowError) as error:
        raise ProofInputError("value cannot be represented as bounded canonical UTF-8 JSON") from error


def canonical_sha256(value: Any) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def expect_keys(value: Mapping[str, Any], required: set[str], *, context: str) -> None:
    actual = set(value)
    if actual != required:
        raise ProofInputError(
            f"{context} fields are not exact: missing={sorted(required - actual)}, unknown={sorted(actual - required)}"
        )


def relative_path(value: Any, *, context: str) -> str:
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise ProofInputError(f"{context} must be a non-empty POSIX relative path")
    parsed = PurePosixPath(value)
    if parsed.is_absolute() or any(part in {"", ".", ".."} for part in parsed.parts):
        raise ProofInputError(f"{context} must not be absolute or contain traversal")
    if re.match(r"^[A-Za-z]:", value) or value.startswith("//"):
        raise ProofInputError(f"{context} must not contain an absolute private path")
    return parsed.as_posix()


def lower_sha256(value: Any, *, context: str) -> str:
    if not isinstance(value, str) or LOWER_SHA256.fullmatch(value) is None:
        raise ProofInputError(f"{context} must be a lowercase SHA-256")
    return value


def positive_int(value: Any, *, context: str, allow_zero: bool = False) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < (0 if allow_zero else 1):
        qualifier = "non-negative" if allow_zero else "positive"
        raise ProofInputError(f"{context} must be a {qualifier} integer")
    return value


def validate_contract(value: dict[str, Any]) -> dict[str, Any]:
    expect_keys(
        value,
        {
            "schema",
            "bundleId",
            "targetPlatform",
            "mediaFileCount",
            "requiredMediaExtensions",
            "audiobookMinimumDurationMilliseconds",
            "helperToolchain",
            "files",
            "databaseContracts",
        },
        context="contract",
    )
    if value["schema"] != 1 or value["bundleId"] != "cortex-owner-product-proof-inputs-v1":
        raise ProofInputError("contract identity is unsupported")
    if value["targetPlatform"] != "windows-11-x64-owner":
        raise ProofInputError("proof-input contract must target the owner Windows workstation")
    if value["mediaFileCount"] != 3 or value["requiredMediaExtensions"] != [".flac", ".mov", ".mp4"]:
        raise ProofInputError("media contract must require exactly FLAC, MOV, and MP4")
    audiobook_duration = positive_int(value["audiobookMinimumDurationMilliseconds"], context="audiobook duration")
    if audiobook_duration <= 60_000:
        raise ProofInputError("long-form audiobook authority must exceed 60 seconds")

    toolchain = value["helperToolchain"]
    if not isinstance(toolchain, dict):
        raise ProofInputError("helperToolchain must be an object")
    expect_keys(
        toolchain,
        {
            "channel",
            "cargoBinarySha256",
            "rustcBinarySha256",
            "gitBinarySha256",
            "gitVersion",
            "cargoConfigSha256",
            "cargoCommitHash",
            "rustcCommitHash",
            "msvcToolsVersion",
            "windowsSdkVersion",
            "clBinarySha256",
            "linkBinarySha256",
            "libBinarySha256",
            "rcBinarySha256",
            "mtBinarySha256",
            "msvcTreeSha256",
            "windowsSdkTreeSha256",
            "rustRuntimeTreeSha256",
            "gitRuntimeTreeSha256",
        },
        context="helperToolchain",
    )
    if toolchain["channel"] != "1.95.0-x86_64-pc-windows-msvc":
        raise ProofInputError("owner-proof helper toolchain channel is not the pinned Windows authority")
    for field in TOOLCHAIN_HASH_FIELDS:
        lower_sha256(toolchain[field], context=f"helperToolchain {field}")
    for field in ("msvcToolsVersion", "windowsSdkVersion"):
        if not isinstance(toolchain[field], str) or re.fullmatch(r"[0-9]+(?:\.[0-9]+){2,3}", toolchain[field]) is None:
            raise ProofInputError(f"helperToolchain {field} must be an exact dotted version")
    if not isinstance(toolchain["gitVersion"], str) or not toolchain["gitVersion"].startswith("git version "):
        raise ProofInputError("helperToolchain gitVersion must be an exact Git version identity")
    for field in ("cargoCommitHash", "rustcCommitHash"):
        if not isinstance(toolchain[field], str) or FULL_GIT_SHA.fullmatch(toolchain[field]) is None:
            raise ProofInputError(f"helperToolchain {field} must be a full commit hash")

    files = value["files"]
    if not isinstance(files, list) or len(files) != len(SOURCE_ROLES):
        raise ProofInputError("contract must declare exactly six source authorities")
    by_role: dict[str, dict[str, Any]] = {}
    paths: set[str] = set()
    for index, item in enumerate(files):
        if not isinstance(item, dict):
            raise ProofInputError(f"contract file {index} must be an object")
        required = {"role", "relativePath", "sourceBasename", "sha256"}
        allowed = required | {"sizeBytes"}
        if not required.issubset(item) or not set(item).issubset(allowed):
            raise ProofInputError(f"contract file {index} fields are not exact")
        role = item["role"]
        if role not in SOURCE_ROLES or role in by_role:
            raise ProofInputError(f"contract source role is missing, duplicated, or unknown: {role!r}")
        relative = relative_path(item["relativePath"], context=f"{role} relativePath")
        if relative in paths:
            raise ProofInputError("contract source paths must be unique")
        source_basename = item["sourceBasename"]
        if not isinstance(source_basename, str) or source_basename != Path(source_basename).name:
            raise ProofInputError(f"{role} sourceBasename must be one filename")
        lower_sha256(item["sha256"], context=f"{role} sha256")
        if "sizeBytes" in item:
            positive_int(item["sizeBytes"], context=f"{role} sizeBytes")
        by_role[role] = item
        paths.add(relative)
    if tuple(sorted(by_role)) != tuple(sorted(SOURCE_ROLES)):
        raise ProofInputError("contract source-role inventory is incomplete")
    media_extensions = sorted(Path(by_role[role]["relativePath"]).suffix.casefold() for role in MEDIA_ROLES)
    if media_extensions != value["requiredMediaExtensions"]:
        raise ProofInputError("contract media extension inventory is inconsistent")
    if Path(by_role["real-media-mp4"]["relativePath"]).name != "A1-0001_PODCAST-001.mp4":
        raise ProofInputError("the exact root MP4 required by the Kurdish VAD test is missing")
    if Path(by_role["long-audiobook-mp3"]["relativePath"]).suffix.casefold() != ".mp3":
        raise ProofInputError("the long audiobook authority must be MP3")

    databases = value["databaseContracts"]
    if not isinstance(databases, dict):
        raise ProofInputError("databaseContracts must be an object")
    expect_keys(databases, {"scale", "campaignExact"}, context="databaseContracts")
    scale = databases["scale"]
    campaign = databases["campaignExact"]
    if not isinstance(scale, dict) or not isinstance(campaign, dict):
        raise ProofInputError("database contracts must be objects")
    expect_keys(
        scale,
        {
            "authorityRole",
            "sourceSchemaVersion",
            "targetSchemaVersion",
            "segmentCount",
            "distinctAudioPathCount",
            "campaignAuthority",
            "sourceSchemaFingerprintSha256",
            "targetSchemaFingerprintSha256",
            "derivedRelativePath",
        },
        context="scale database contract",
    )
    expect_keys(
        campaign,
        {
            "authorityRole",
            "schemaVersion",
            "segmentCount",
            "distinctAudioPathCount",
            "campaignAuthority",
            "schemaFingerprintSha256",
        },
        context="campaign database contract",
    )
    if scale["authorityRole"] != "scale-database-authority" or campaign["authorityRole"] != "campaign-database-authority":
        raise ProofInputError("database authority roles are inconsistent")
    source_schema = positive_int(scale["sourceSchemaVersion"], context="scale source schema")
    target_schema = positive_int(scale["targetSchemaVersion"], context="scale target schema")
    if target_schema <= source_schema:
        raise ProofInputError("scale target schema must be newer than its source")
    positive_int(scale["segmentCount"], context="scale segment count")
    positive_int(scale["distinctAudioPathCount"], context="scale distinct source count")
    positive_int(campaign["schemaVersion"], context="campaign schema")
    positive_int(campaign["segmentCount"], context="campaign segment count")
    positive_int(campaign["distinctAudioPathCount"], context="campaign distinct source count")
    if scale["campaignAuthority"] != "absent" or campaign["campaignAuthority"] != "required":
        raise ProofInputError("scale must be campaign-free and the characterization clone must retain campaign authority")
    for field in ("sourceSchemaFingerprintSha256", "targetSchemaFingerprintSha256"):
        lower_sha256(scale[field], context=f"scale {field}")
    lower_sha256(campaign["schemaFingerprintSha256"], context="campaign schemaFingerprintSha256")
    derived = relative_path(scale["derivedRelativePath"], context="scale derivedRelativePath")
    if derived in paths or not derived.startswith("db-derived/") or not derived.endswith(".db"):
        raise ProofInputError("derived scale database path is invalid or collides with an authority")
    return value
