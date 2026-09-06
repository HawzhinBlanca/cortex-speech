#!/usr/bin/env python3
"""Build a deterministic, read-only SUPERSEDING duplicate-exclusion manifest (schema 2) for the active pool.

The source database is opened with SQLite ``mode=ro``. The output is an atomic staging artifact; it
does not alter pool membership, reviewer history, or any audio. ``pool_admin apply-dedup`` (schema 70+)
revalidates every frozen member identity, every proof edge and the canonical selection before it
appends the exclusions this manifest introduces.

Why schema 2 (2026-09-06): the v1 verdict compared waveforms at zero lag with a 0.98 bar and cleared
every twin whose cut differed by a few milliseconds. Measured on the live pool, 88% of 8,909
text-matched cross-file pairs were the same recording at the best lag. A superseding manifest states
the COMPLETE dedup state — every applied family reproduced under its applied canonical, new members
and new families appended — so the database can prove nothing was moved or lost, and it may retire a
clip that already carries review evidence (the evidence stays; the clip leaves serving and export).

Pairs that score above the control ceiling but below the confirmed bar are written to a sidecar
listening list and counted in the manifest; they are never excluded by machine.
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
from collections import defaultdict
from pathlib import Path

from check_dataset_duplicates import (
    AUDIO_DUPLICATE_CORRELATION,
    AUDIO_DURATION_TOLERANCE_MS,
    AUDIO_MAX_LAG_MS,
    AUDIO_MIN_OVERLAP_RATIO,
    AUDIO_PROBABLE_CORRELATION,
    OFFSET_BUCKET_MS,
    TEXT_CANDIDATE_SIMILARITY,
    confirm_groups_with_audio,
    duplicate_groups,
)

MANIFEST_SCHEMA = 2
ALGORITHM = "cortex-cross-file-waveform-correlation-v2"
REASON_APPLIED = "preserve-applied-canonical"
REASON_EVIDENCE = "preserve-most-human-review-evidence"
REASON_QUALITY = "best-measured-audio-quality-then-stable-identity"


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


def quality_key(member: dict) -> tuple:
    """The exact stable ordering `dedup.rs::selection_key` derives from the frozen row."""

    def descending(value):
        return (1, 0.0) if value is None else (0, -float(value))

    def ascending(value):
        return (1, 0.0) if value is None else (0, float(value))

    return (
        descending(member["snrMilliDb"]),
        ascending(member["clippingPpm"]),
        ascending(member["signalAnomalyPpm"]),
        descending(member["confidencePpm"]),
        member["sourceFileName"].casefold(),
        member["segmentId"],
    )


def canonical_member(members: list[dict], applied_canonicals: set[str]) -> tuple[dict, str]:
    """Deterministic canonical choice, mirrored exactly by `apply_superseding_manifest`.

    1. An applied canonical stays canonical (its exclusions are immutable rows pointing at it).
    2. Otherwise the member with the MOST human review evidence; an exact tie falls back to the
       stable audio-quality key. The other reviewed twins keep their evidence and leave serving.
    3. Otherwise the best measured audio quality, then stable identity.
    """
    preserved = [member for member in members if member["segmentId"] in applied_canonicals]
    if preserved:
        # Two applied v1 families the lag-tolerant verdict proves to be one recording merge here: the
        # applied canonical with the most human evidence stays (tie: stable quality key), the other
        # retires under it; its own exclusion rows keep pointing at it and the binding follows the chain.
        most = max(member["reviewEvidenceCount"] for member in preserved)
        tied = [member for member in preserved if member["reviewEvidenceCount"] == most]
        return min(tied, key=quality_key), REASON_APPLIED
    most = max(member["reviewEvidenceCount"] for member in members)
    if most > 0:
        tied = [member for member in members if member["reviewEvidenceCount"] == most]
        return min(tied, key=quality_key), REASON_EVIDENCE
    return min(members, key=quality_key), REASON_QUALITY


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


def _table_exists(connection: sqlite3.Connection, name: str) -> bool:
    return (
        connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone()
        is not None
    )


def _connected(segment_ids: list[str], edges: list[dict]) -> bool:
    parent = {sid: sid for sid in segment_ids}

    def find(x: str) -> str:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for edge in edges:
        left, right = find(edge["leftSegmentId"]), find(edge["rightSegmentId"])
        if left != right:
            parent[right] = left
    roots = {find(sid) for sid in segment_ids}
    return len(roots) == 1


def build_manifest(database: Path) -> tuple[dict, dict]:
    """Return (manifest, sidecar). The sidecar holds the probable pairs and cross-voice risks."""
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
        base = connection.execute(
            "SELECT manifest_sha256 FROM review_pool_dedup_manifests WHERE pool_id=?", (pool_id,)
        ).fetchone()
        if base is None:
            raise ValueError(
                "a superseding manifest requires an applied v1 base manifest; this pool has none"
            )
        supersedes = base["manifest_sha256"]
        if _table_exists(connection, "review_pool_dedup_supersessions"):
            latest = connection.execute(
                """SELECT manifest_sha256 FROM review_pool_dedup_supersessions
                    WHERE pool_id=? ORDER BY sequence DESC LIMIT 1""",
                (pool_id,),
            ).fetchone()
            if latest is not None:
                supersedes = latest["manifest_sha256"]
        applied: dict[str, str] = {
            row["segment_id"]: row["canonical_segment_id"]
            for row in connection.execute(
                "SELECT segment_id, canonical_segment_id FROM review_pool_duplicate_exclusions WHERE pool_id=?",
                (pool_id,),
            )
        }
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
    confirmed, unconfirmed, repeats, proof, probable = confirm_groups_with_audio(
        candidates, audit_rows, include_proof=True, include_probable=True
    )
    if unconfirmed:
        raise ValueError(f"duplicate audit has {len(unconfirmed)} unresolved cross-file risk groups (missing audio or non-identical complete clips); keep these clips for review")
    if len(confirmed) != len(proof):
        raise ValueError("duplicate proof/component cardinality mismatch")

    source_by_id = {row["id"]: row for row in source_rows}
    applied_canonicals = set(applied.values())
    proof_by_members = {
        frozenset(segment_id for segment_id, _ in item["members"]): item["edges"] for item in proof
    }
    families = []
    cross_voice_groups: list[list[str]] = []
    excluded_ids: dict[str, str] = {}
    reviewed_canonical_count = 0
    excluded_reviewed_count = 0
    retired_applied_canonicals = 0
    for component in confirmed:
        component_ids = sorted(segment_id for segment_id, _ in component)
        voices = {source_by_id[segment_id]["voice_name"] for segment_id in component_ids}
        if len(voices) != 1:
            # The same recording under two voice labels is a labelling fault, not a dedup decision:
            # no machine may pick which voice it "really" is. Surfaced, never excluded.
            cross_voice_groups.append(component_ids)
            continue
        component_edges = proof_by_members[frozenset(component_ids)]
        for segment_ids in (component_ids,):
            edges = sorted(component_edges, key=lambda edge: (edge["leftSegmentId"], edge["rightSegmentId"]))
            if not _connected(segment_ids, edges):
                raise ValueError(f"duplicate family {segment_ids} is not waveform-connected")
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
            canonical, selection_reason = canonical_member(members, applied_canonicals)
            member_set = set(segment_ids)
            for segment_id in segment_ids:
                if segment_id in applied and applied[segment_id] not in member_set:
                    raise ValueError(
                        f"applied exclusion {segment_id} is separated from its canonical {applied[segment_id]}"
                    )
            retired_applied_canonicals += sum(
                1
                for segment_id in segment_ids
                if segment_id in applied_canonicals and segment_id != canonical["segmentId"]
            )
            if canonical["reviewEvidenceCount"]:
                reviewed_canonical_count += 1
            family_material = {"poolId": pool_id, "segmentIds": segment_ids, "proofEdges": edges}
            family_id = hashlib.sha256(canonical_json(family_material)).hexdigest()
            for member in members:
                member["canonical"] = member["segmentId"] == canonical["segmentId"]
                if not member["canonical"]:
                    excluded_ids[member["segmentId"]] = canonical["segmentId"]
                    if member["reviewEvidenceCount"]:
                        excluded_reviewed_count += 1
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

    # An applied exclusion is restated when it is excluded again inside the family that also holds
    # its applied canonical (which may itself be retired there).
    missing = sorted(sid for sid in applied if sid not in excluded_ids)
    if missing:
        raise ValueError(f"superseding manifest would drop or move applied exclusions: {missing[:10]}")
    newly_excluded = sorted(sid for sid in excluded_ids if sid not in applied)

    families.sort(key=lambda family: family["familyId"])
    payload = {
        "manifestSchema": MANIFEST_SCHEMA,
        "supersedes": {"manifestSha256": supersedes},
        "algorithm": {
            "id": ALGORITHM,
            "minimumTextCharacters": 25,
            "offsetToleranceMs": OFFSET_BUCKET_MS,
            "minimumTextSimilarityPpm": 900_000,
            "textCandidateSimilarityPpm": round(TEXT_CANDIDATE_SIMILARITY * 1_000_000),
            "audioDurationToleranceMs": AUDIO_DURATION_TOLERANCE_MS,
            "maximumLagMs": AUDIO_MAX_LAG_MS,
            "minimumOverlapPpm": round(AUDIO_MIN_OVERLAP_RATIO * 1_000_000),
            "minimumWaveformCorrelationPpm": round(AUDIO_DUPLICATE_CORRELATION * 1_000_000),
            "probableWaveformCorrelationPpm": round(AUDIO_PROBABLE_CORRELATION * 1_000_000),
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
            "excludedMembers": len(excluded_ids),
            "canonicalMembers": len(source_rows) - len(excluded_ids),
            "unconfirmedRiskGroups": 0,
            "reviewedCanonicalMembers": reviewed_canonical_count,
            "newlyExcludedMembers": len(newly_excluded),
            "excludedReviewedMembers": excluded_reviewed_count,
            "probableDuplicatePairs": len(probable),
            "crossVoiceRiskGroups": len(cross_voice_groups),
            "retiredAppliedCanonicals": retired_applied_canonicals,
        },
        "families": families,
        "generatedAtMs": int(time.time() * 1000),
    }
    payload["manifestSha256"] = hashlib.sha256(canonical_json(payload)).hexdigest()
    sidecar = {
        "manifestSha256": payload["manifestSha256"],
        "probablePairs": [
            {
                "leftSegmentId": left,
                "rightSegmentId": right,
                "leftFile": os.path.basename(source_by_id[left]["audio_path"]),
                "rightFile": os.path.basename(source_by_id[right]["audio_path"]),
                "correlation": round(score, 4),
            }
            for left, right, score in probable
        ],
        "crossVoiceRiskGroups": [
            [
                {"segmentId": sid, "voiceName": source_by_id[sid]["voice_name"], "file": os.path.basename(source_by_id[sid]["audio_path"])}
                for sid in group
            ]
            for group in cross_voice_groups
        ],
        "newlyExcludedReviewedMembers": sorted(
            sid for sid in newly_excluded if evidence_counts.get(sid, 0)
        ),
        "retiredAppliedCanonicals": sorted(sid for sid in newly_excluded if sid in applied_canonicals),
    }
    return payload, sidecar


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
    parser.add_argument("--sidecar", type=Path, default=None, help="probable/cross-voice listening list (default: <output>.review.json)")
    parser.add_argument("--replace", action="store_true")
    args = parser.parse_args()
    if not args.db.is_file():
        parser.error(f"database does not exist: {args.db}")
    manifest, sidecar = build_manifest(args.db.resolve())
    output = args.output.resolve()
    write_atomic(output, manifest, args.replace)
    sidecar_path = (args.sidecar or output.with_name(output.stem + ".review.json")).resolve()
    write_atomic(sidecar_path, sidecar, True)
    print(
        json.dumps(
            {
                "ok": True,
                "output": str(output),
                "sidecar": str(sidecar_path),
                "manifestSha256": manifest["manifestSha256"],
                "supersedes": manifest["supersedes"]["manifestSha256"],
                **manifest["summary"],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
