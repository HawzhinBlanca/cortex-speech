#!/usr/bin/env python3
"""Build a deterministic, read-only duplicate-exclusion manifest for the active review pool.

The source database is opened with SQLite ``mode=ro``. The output is an atomic staging artifact; it
does not alter pool membership, reviewer history, or any audio. A later schema-aware operator command
must revalidate every frozen member identity before it may apply the manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sqlite3
import sys
import time
from pathlib import Path

from check_dataset_duplicates import (
    AUDIO_DUPLICATE_CORRELATION,
    AUDIO_DURATION_TOLERANCE_MS,
    OFFSET_BUCKET_MS,
    confirm_groups_with_audio,
    duplicate_groups,
)

MANIFEST_SCHEMA = 1
ALGORITHM = "cortex-cross-file-waveform-correlation-v1"


def canonical_json(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def text_sha256(value: str) -> str:
    normalized = " ".join((value or "").split())
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def scaled_metric(value: float | None, multiplier: int) -> int | None:
    """Match Rust f64::round so manifest validation is deterministic across languages."""
    if value is None or not math.isfinite(value):
        return None
    scaled = value * multiplier
    return math.floor(scaled + 0.5) if scaled >= 0 else math.ceil(scaled - 0.5)


def canonical_member(members: list[dict]) -> tuple[dict, str]:
    reviewed = [member for member in members if member["reviewEvidenceCount"] > 0]
    if len(reviewed) > 1:
        ids = ", ".join(sorted(member["segmentId"] for member in reviewed))
        raise ValueError(f"duplicate family has review evidence on multiple members: {ids}")
    if reviewed:
        return reviewed[0], "preserve-human-review-evidence"

    def descending(value):
        return (1, 0.0) if value is None else (0, -float(value))

    def ascending(value):
        return (1, 0.0) if value is None else (0, float(value))

    selected = min(
        members,
        key=lambda member: (
            descending(member["snrMilliDb"]),
            ascending(member["clippingPpm"]),
            ascending(member["signalAnomalyPpm"]),
            descending(member["confidencePpm"]),
            member["sourceFileName"].casefold(),
            member["segmentId"],
        ),
    )
    return selected, "best-measured-audio-quality-then-stable-identity"


def _evidence_counts(connection: sqlite3.Connection) -> dict[str, int]:
    counts: dict[str, int] = {}
    queries = [
        """SELECT member.segment_id
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE segment.verified=1
                AND segment.human_decision IN
                    ('accept','edit','reject','human_accept','human_edit','human_reject')""",
        "SELECT segment_id FROM effective_review_pool_decisions_v62",
        """SELECT decision.segment_id
               FROM effective_independent_review_decisions_v61 decision
               JOIN review_pool_members member ON member.segment_id=decision.segment_id""",
        "SELECT segment_id FROM review_pool_owner_adjudications",
    ]
    for query in queries:
        for (segment_id,) in connection.execute(query):
            counts[segment_id] = counts.get(segment_id, 0) + 1
    return counts


def build_manifest(database: Path) -> dict:
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        registry = connection.execute(
            """SELECT pool_id, focus_segment_count, focus_sha256,
                      champion_model_version_id, champion_deployment_sha256
                 FROM review_pool_registry WHERE singleton_key=1"""
        ).fetchone()
        if registry is None:
            raise ValueError("active review pool registry is absent")
        pool_id = registry["pool_id"]
        source_rows = connection.execute(
            """SELECT segment.id, segment.audio_path, pool.raw_transcript, pool.voice_name,
                      pool.audio_content_hash, pool.source_start_ms, pool.source_end_ms,
                      pool.duration_ms, segment.verified, segment.snr_db, segment.clipping_ratio,
                      segment.signal_anomaly_score, segment.confidence
                 FROM review_pool_members pool
                 JOIN speech_segments segment ON segment.id=pool.segment_id
                WHERE pool.pool_id=?
                ORDER BY segment.id""",
            (pool_id,),
        ).fetchall()
        if len(source_rows) != registry["focus_segment_count"]:
            raise ValueError(
                f"pool registry expects {registry['focus_segment_count']} members, found {len(source_rows)}"
            )
        evidence_counts = _evidence_counts(connection)
    finally:
        connection.close()

    audit_rows = [
        (
            row["id"],
            row["audio_path"],
            json.dumps(
                {"source_start_ms": row["source_start_ms"], "source_end_ms": row["source_end_ms"]},
                separators=(",", ":"),
            ),
            row["raw_transcript"],
            row["verified"],
        )
        for row in source_rows
    ]
    candidates = duplicate_groups(audit_rows)
    confirmed, unconfirmed, repeats, proof = confirm_groups_with_audio(
        candidates, audit_rows, include_proof=True
    )
    if unconfirmed:
        raise ValueError(f"duplicate audit has {len(unconfirmed)} unreadable cross-file risk groups")
    if len(confirmed) != len(proof):
        raise ValueError("duplicate proof/component cardinality mismatch")

    source_by_id = {row["id"]: row for row in source_rows}
    proof_by_members = {
        frozenset(segment_id for segment_id, _ in item["members"]): item["edges"] for item in proof
    }
    families = []
    excluded_count = 0
    reviewed_canonical_count = 0
    for component in confirmed:
        segment_ids = sorted(segment_id for segment_id, _ in component)
        voices = {source_by_id[segment_id]["voice_name"] for segment_id in segment_ids}
        if len(voices) != 1:
            raise ValueError(f"duplicate family crosses voice identities: {segment_ids}")
        members = []
        for segment_id in segment_ids:
            row = source_by_id[segment_id]
            members.append(
                {
                    "segmentId": segment_id,
                    "voiceName": row["voice_name"],
                    "sourceFileName": os.path.basename(row["audio_path"]),
                    "rawTranscriptSha256": text_sha256(row["raw_transcript"]),
                    "audioContentHash": row["audio_content_hash"],
                    "sourceStartMs": row["source_start_ms"],
                    "sourceEndMs": row["source_end_ms"],
                    "durationMs": row["duration_ms"],
                    "reviewEvidenceCount": evidence_counts.get(segment_id, 0),
                    "snrMilliDb": scaled_metric(row["snr_db"], 1_000),
                    "clippingPpm": scaled_metric(row["clipping_ratio"], 1_000_000),
                    "signalAnomalyPpm": scaled_metric(row["signal_anomaly_score"], 1_000_000),
                    "confidencePpm": scaled_metric(row["confidence"], 1_000_000),
                }
            )
        canonical, selection_reason = canonical_member(members)
        if canonical["reviewEvidenceCount"]:
            reviewed_canonical_count += 1
        edges = sorted(
            proof_by_members[frozenset(segment_ids)],
            key=lambda edge: (edge["leftSegmentId"], edge["rightSegmentId"]),
        )
        family_material = {
            "poolId": pool_id,
            "segmentIds": segment_ids,
            "proofEdges": edges,
        }
        family_id = hashlib.sha256(canonical_json(family_material)).hexdigest()
        for member in members:
            member["canonical"] = member["segmentId"] == canonical["segmentId"]
        families.append(
            {
                "familyId": family_id,
                "voiceName": next(iter(voices)),
                "canonicalSegmentId": canonical["segmentId"],
                "canonicalSelectionReason": selection_reason,
                "members": members,
                "proofEdges": edges,
            }
        )
        excluded_count += len(members) - 1

    families.sort(key=lambda family: family["familyId"])
    payload = {
        "manifestSchema": MANIFEST_SCHEMA,
        "algorithm": {
            "id": ALGORITHM,
            "minimumTextCharacters": 25,
            "offsetToleranceMs": OFFSET_BUCKET_MS,
            "minimumTextSimilarityPpm": 900_000,
            "audioDurationToleranceMs": AUDIO_DURATION_TOLERANCE_MS,
            "minimumWaveformCorrelationPpm": round(AUDIO_DUPLICATE_CORRELATION * 1_000_000),
            "comparisonSampleRateHz": 16_000,
        },
        "pool": {
            "poolId": pool_id,
            "sourceFocusSegmentCount": registry["focus_segment_count"],
            "sourceFocusSha256": registry["focus_sha256"],
            "championModelVersionId": registry["champion_model_version_id"],
            "championDeploymentSha256": registry["champion_deployment_sha256"],
        },
        "summary": {
            "candidateTextGroups": len(candidates),
            "clearedRepeatedTextGroups": len(repeats),
            "duplicateFamilies": len(families),
            "excludedMembers": excluded_count,
            "canonicalMembers": len(source_rows) - excluded_count,
            "unconfirmedRiskGroups": 0,
            "reviewedCanonicalMembers": reviewed_canonical_count,
        },
        "families": families,
        "generatedAtMs": int(time.time() * 1000),
    }
    payload["manifestSha256"] = hashlib.sha256(canonical_json(payload)).hexdigest()
    return payload


def write_atomic(output: Path, manifest: dict, replace: bool) -> None:
    if output.exists() and not replace:
        raise ValueError(f"output already exists (pass --replace explicitly): {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.{os.getpid()}.tmp")
    try:
        with temporary.open("wb") as handle:
            handle.write(json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2).encode("utf-8"))
            handle.write(b"\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replace", action="store_true")
    args = parser.parse_args()
    if not args.db.is_file():
        parser.error(f"database does not exist: {args.db}")
    manifest = build_manifest(args.db.resolve())
    write_atomic(args.output.resolve(), manifest, args.replace)
    print(
        json.dumps(
            {
                "ok": True,
                "output": str(args.output.resolve()),
                "manifestSha256": manifest["manifestSha256"],
                **manifest["summary"],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
