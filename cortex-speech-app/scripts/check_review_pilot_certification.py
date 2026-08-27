#!/usr/bin/env python3
"""Fail-closed final certificate for the active review mode.

Flexible schema-65 production intentionally has no hidden-check pilot. In that mode this gate
validates the immutable active release, runs its exact hash-bound ``pool_admin certify`` binary on a
detached database copy, and requires review-ready database/audio/rights/disk/snapshot authority. It
does not confuse review readiness with a completed dataset.

Without an active flexible pool, the original controlled-pilot contract remains unchanged: exactly
ten corpus decisions and two hidden-QC decisions per reviewer, zero skips, hidden 2/2 at CER 0,
playback evidence for all 24 UI decisions, and one valid compensation ledger consequence/operation
receipt per decision. Neither path claims or leases work or writes the live database, session,
focus, policy, app, model service, or GPU state.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sqlite3
import sys
import unicodedata
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from check_exe_freshness import extract_baked_sha

from check_playback_enforcement_readiness import (
    CONTENT_HASH_ONLY_PLAYBACK_POLICY_VERSION,
    LEGACY_PLAYBACK_POLICY_VERSION,
    MIN_PLAYBACK_COVERAGE,
    PLAYBACK_POLICY_VERSION,
    binary_can_warn,
    canonical_source_span,
    canonical_receipt_coverage,
    is_canonical_audio_content_hash,
    playback_receipt_semantic_issues,
    receipt_source_span_issue,
    source_span_duration_issue,
)
from check_review_compensation_readiness import POLICY_VERSION, audit as audit_compensation
from check_spot_check_pool import PolicyBroken, active_flexible_pool as structurally_active_flexible_pool
from pilot_focus_contract import (
    CANONICAL_IDENTITY_KIND,
    PLAYBACK_GUARD_VERSION,
    PilotFocusError,
    PilotFocusEvidence,
    canonical_reviewer_work_id,
    load_voice_focus_ids,
    verify_controlled_pilot_focus,
)
from review_pilot_hidden_contract import (
    CORPUS_ACTIONS_PER_REVIEWER,
    HIDDEN_KEYS_PER_REVIEWER,
    MAX_UI_ACTIONS,
    PILOT_REVIEWERS,
    REQUIRED_SCHEMA,
    TOTAL_CORPUS_ACTIONS,
    TOTAL_HIDDEN_KEYS,
    HiddenPilotState,
    PilotContractError,
    ReviewPilotPolicy,
    audit_active_hidden_state,
    audit_pilot_review_history,
    policy_sha256,
    read_policy,
)
from release_private_production import EXPECTED_SCHEMA, ReleaseError, active_pointer, run_json


APP_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_EXE = APP_ROOT / "src-tauri" / "target" / "release" / "cortex-speech-app.exe"
ALLOWED_ACTIONS = {"accept", "edit", "reject"}
ALLOWED_SOURCES = {"couch", "couch_spot_check"}


@dataclass(frozen=True)
class ReviewEventEvidence:
    event_id: int
    segment_id: str
    reviewer: str
    action: str
    source: str
    created_at: str
    timestamp_ms: object
    duration_ms: object = 1000
    compensation_action: str = "accept"
    operation_id: str = "11111111-1111-4111-8111-111111111111"
    operation_payload_hash: str = "a" * 64
    app_git_sha: str = "a" * 40
    playback_guard_version: str = PLAYBACK_GUARD_VERSION
    ledger_id: int = 0
    ledger_entry_id: str = ""
    canonical_work_id: str = ""
    canonical_identity_kind: str = CANONICAL_IDENTITY_KIND
    decision_revision: object = 1


def default_data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if not appdata:
        raise RuntimeError("APPDATA is not set; pass --data-dir explicitly")
    return Path(appdata) / "cortex-speech"


def default_release_root() -> Path:
    localappdata = os.environ.get("LOCALAPPDATA")
    if not localappdata:
        raise RuntimeError("LOCALAPPDATA is not set; pass --release-root explicitly")
    return Path(localappdata) / "CortexSpeech" / "private-production-releases"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def connect_read_only(path: Path) -> sqlite3.Connection:
    connection = sqlite3.connect(f"file:{path.resolve().as_posix()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA query_only = ON")
    connection.execute("BEGIN")
    return connection


def _exact_nonnegative_int(value: object) -> bool:
    return type(value) is int and value >= 0


FlexiblePoolIdentity = tuple[str, int, str, str, str]


def active_flexible_pool_identity(connection: sqlite3.Connection) -> FlexiblePoolIdentity | None:
    """Add champion identity to the independently checked structural pool authority."""

    structural = structurally_active_flexible_pool(connection)
    if structural is None:
        return None
    pool_id, clip_count, focus_sha256 = structural
    rows = connection.execute(
        "SELECT champion_model_version_id, champion_deployment_sha256 "
        "FROM review_pool_registry WHERE pool_id=?",
        (pool_id,),
    ).fetchall()
    if len(rows) != 1:
        raise PolicyBroken("flexible review-pool champion identity is not uniquely bound")
    model_id, deployment_sha256 = rows[0]
    if not isinstance(model_id, str) or not model_id.strip():
        raise PolicyBroken("flexible review-pool champion model identity is blank")
    if (
        not isinstance(deployment_sha256, str)
        or len(deployment_sha256) != 64
        or any(character not in "0123456789abcdef" for character in deployment_sha256)
    ):
        raise PolicyBroken("flexible review-pool champion deployment digest is invalid")
    return pool_id, clip_count, focus_sha256, model_id, deployment_sha256


def flexible_report_issues(
    report: dict[str, Any],
    pool: FlexiblePoolIdentity,
    manifest: dict[str, Any],
) -> list[str]:
    """Refuse a syntactically green report whose internal authority is inconsistent."""

    errors: list[str] = []
    pool_id, clip_count, focus_sha256, champion_model_id, champion_deployment_sha256 = pool

    def mapping(name: str) -> dict[str, Any]:
        value = report.get(name)
        if not isinstance(value, dict):
            errors.append(f"certification {name} is not one object")
            return {}
        return value

    if report.get("reportSchema") != 3:
        errors.append("certification report schema is not exactly 3")
    if report.get("readOnly") is not True:
        errors.append("certification does not identify itself as read-only")
    if not _exact_nonnegative_int(report.get("generatedAtEpochSecs")) or report.get("generatedAtEpochSecs") == 0:
        errors.append("certification generation time is invalid")
    if report.get("appGitSha") != manifest.get("appGitSha"):
        errors.append("certification and active immutable release git identities disagree")
    if report.get("databaseSchemaVersion") != EXPECTED_SCHEMA:
        errors.append(f"certification database schema is not exactly {EXPECTED_SCHEMA}")

    dedup = mapping("dedup")
    canonical_count = dedup.get("canonicalSegmentCount")
    excluded_count = dedup.get("excludedSegmentCount")
    if (
        dedup.get("applied") is not True
        or dedup.get("algorithmId") != "cortex-cross-file-waveform-correlation-v1"
        or dedup.get("manifestSha256") != manifest.get("dedupManifestSha256")
        or dedup.get("sourceSegmentCount") != clip_count
        or not _exact_nonnegative_int(canonical_count)
        or not _exact_nonnegative_int(excluded_count)
        or not _exact_nonnegative_int(dedup.get("duplicateFamilyCount"))
        or dedup.get("unconfirmedRiskCount") != 0
        or (
            _exact_nonnegative_int(canonical_count)
            and _exact_nonnegative_int(excluded_count)
            and canonical_count + excluded_count != clip_count
        )
    ):
        errors.append("certification duplicate-exclusion authority is incomplete or inconsistent")
    canonical_count = canonical_count if _exact_nonnegative_int(canonical_count) else -1

    pool_report = mapping("pool")
    if (
        pool_report.get("poolId") != pool_id
        or pool_report.get("focusSegmentCount") != clip_count
        or pool_report.get("focusSha256") != focus_sha256
        or pool_report.get("reviewSegmentCount") != canonical_count
        or pool_report.get("excludedDuplicateCount") != excluded_count
        or pool_report.get("duplicateFamilyCount") != dedup.get("duplicateFamilyCount")
        or pool_report.get("dedupManifestSha256") != dedup.get("manifestSha256")
        or pool_report.get("championModelVersionId") != champion_model_id
        or pool_report.get("championDeploymentSha256") != champion_deployment_sha256
    ):
        errors.append("certification pool identity does not match the live immutable registry")

    summary = mapping("resolutionSummary")
    summary_fields = (
        "totalClips",
        "resolvedClips",
        "needsFirstOrSecondReview",
        "needsThirdReview",
        "ownerConflicts",
    )
    if not all(_exact_nonnegative_int(summary.get(field)) for field in summary_fields):
        errors.append("certification resolution totals are not exact non-negative integers")
        summary_values = None
    else:
        summary_values = {field: int(summary[field]) for field in summary_fields}
        if summary_values["totalClips"] != canonical_count:
            errors.append("certification resolution total does not match canonical pool membership")
        classified = (
            summary_values["resolvedClips"]
            + summary_values["needsFirstOrSecondReview"]
            + summary_values["needsThirdReview"]
            + summary_values["ownerConflicts"]
        )
        if classified != summary_values["totalClips"]:
            errors.append("certification resolution categories do not exactly partition the pool")

    authority = mapping("resolutionAuthority")
    authority_fields = ("consensusAgreements", "ownerAdjudications", "unresolvedConflicts")
    if not all(_exact_nonnegative_int(authority.get(field)) for field in authority_fields):
        errors.append("certification resolution authority totals are invalid")
    elif summary_values is not None:
        if authority["consensusAgreements"] + authority["ownerAdjudications"] != summary_values["resolvedClips"]:
            errors.append("certification resolved total disagrees with consensus/owner authority")
        if authority["unresolvedConflicts"] != summary_values["ownerConflicts"]:
            errors.append("certification conflict totals disagree")

    coverage = report.get("coverageByVoice")
    if not isinstance(coverage, list) or not coverage:
        errors.append("certification has no per-voice coverage")
    else:
        voice_total = 0
        names: set[str] = set()
        coverage_totals: dict[str, int] = {}
        for row in coverage:
            if not isinstance(row, dict):
                errors.append("certification has a malformed per-voice coverage row")
                continue
            name = row.get("voiceName")
            total = row.get("totalClips")
            if not isinstance(name, str) or not name.strip() or name in names:
                errors.append("certification per-voice identities are blank or duplicated")
            else:
                names.add(name)
            if not _exact_nonnegative_int(total):
                errors.append("certification per-voice clip total is invalid")
            else:
                voice_total += total
                if isinstance(name, str) and name.strip():
                    coverage_totals[name] = total
            review_buckets = ("zeroReviews", "oneReview", "twoReviews", "threeOrMoreReviews")
            if not all(_exact_nonnegative_int(row.get(field)) for field in review_buckets):
                errors.append("certification per-voice review buckets are invalid")
            elif _exact_nonnegative_int(total) and sum(row[field] for field in review_buckets) != total:
                errors.append("certification per-voice review buckets do not partition the voice")
        if voice_total != canonical_count:
            errors.append("certification per-voice coverage does not exactly cover the canonical pool")

        outcomes = report.get("voiceOutcomes")
        if not isinstance(outcomes, dict) or set(outcomes) != names:
            errors.append("certification voice outcomes do not match per-voice coverage")
        else:
            for name, row in outcomes.items():
                if not isinstance(row, dict):
                    errors.append(f"certification outcome for {name} is malformed")
                    continue
                numeric = (
                    "total",
                    "retained",
                    "rejected",
                    "unresolved",
                    "consensusAgreements",
                    "ownerAdjudications",
                    "unresolvedConflicts",
                )
                if not all(_exact_nonnegative_int(row.get(field)) for field in numeric):
                    errors.append(f"certification outcome totals for {name} are invalid")
                    continue
                resolved = row["retained"] + row["rejected"]
                if (
                    row["total"] != coverage_totals.get(name)
                    or resolved + row["unresolved"] != row["total"]
                    or row["consensusAgreements"] + row["ownerAdjudications"] != resolved
                ):
                    errors.append(f"certification outcome totals for {name} are internally inconsistent")

    reviewer_totals = report.get("reviewerVoiceTotals")
    if not isinstance(reviewer_totals, list):
        errors.append("certification reviewer/voice totals are not a list")

    audio = mapping("audio")
    if (
        audio.get("allAvailable") is not True
        or audio.get("clips") != canonical_count
        or audio.get("missingClips") != 0
        or audio.get("missingRecordings") != 0
    ):
        errors.append("certification audio coverage is incomplete or inconsistent")

    rights = mapping("rights")
    if (
        rights.get("allExact") is not True
        or rights.get("exactRows") != clip_count
        or rights.get("segmentRows") != clip_count
        or rights.get("conflictingRows") != 0
        or rights.get("revokedRows") != 0
        or rights.get("unstampedRows") != 0
    ):
        errors.append("certification owner-rights coverage is incomplete or conflicting")

    database = mapping("database")
    if (
        database.get("healthy") is not True
        or database.get("quickCheck") != ["ok"]
        or database.get("fullIntegrityCheck") != ["ok"]
        or database.get("foreignKeyViolations") != 0
    ):
        errors.append("certification database integrity is not fully healthy")

    disk = mapping("disk")
    if (
        disk.get("healthy") is not True
        or not _exact_nonnegative_int(disk.get("freeBytes"))
        or not _exact_nonnegative_int(disk.get("minimumFreeBytes"))
        or disk.get("minimumFreeBytes") == 0
        or (
            _exact_nonnegative_int(disk.get("freeBytes"))
            and _exact_nonnegative_int(disk.get("minimumFreeBytes"))
            and disk["freeBytes"] < disk["minimumFreeBytes"]
        )
    ):
        errors.append("certification writable-disk reserve is unhealthy")

    snapshots = mapping("snapshots")
    for label in ("local", "offsite"):
        snapshot = snapshots.get(label)
        if (
            not isinstance(snapshot, dict)
            or snapshot.get("fresh") is not True
            or snapshot.get("verified") is not True
            or not _exact_nonnegative_int(snapshot.get("ageSecs"))
            or not _exact_nonnegative_int(snapshot.get("targetRpoSecs"))
            or snapshot.get("targetRpoSecs") == 0
            or (
                _exact_nonnegative_int(snapshot.get("ageSecs"))
                and _exact_nonnegative_int(snapshot.get("targetRpoSecs"))
                and snapshot["ageSecs"] > snapshot["targetRpoSecs"]
            )
        ):
            errors.append(f"certification {label} snapshot is not fresh and verified")

    gates = mapping("gates")
    if (
        gates.get("reviewReady") is not True
        or gates.get("rightsComplete") is not True
        or gates.get("duplicateExclusionsBound") is not True
    ):
        errors.append("certification review-readiness gates are not green")
    if summary_values is not None:
        all_resolved = (
            summary_values["resolvedClips"] == canonical_count
            and summary_values["needsFirstOrSecondReview"] == 0
            and summary_values["needsThirdReview"] == 0
            and summary_values["ownerConflicts"] == 0
        )
        if gates.get("allClipsResolved") is not all_resolved:
            errors.append("certification all-resolved gate disagrees with resolution totals")
        final_ready = (
            gates.get("reviewReady") is True
            and all_resolved
            and gates.get("rightsComplete") is True
            and gates.get("everyVoiceCertified") is True
        )
        if gates.get("finalDatasetReady") is not final_ready:
            errors.append("certification final-ready gate disagrees with its component authority")
    return errors


def certify_flexible_pool(
    data_dir: Path,
    db_path: Path,
    release_root: Path,
    pool: FlexiblePoolIdentity,
    *,
    explicit_exe: Path | None = None,
) -> int:
    """Certify the active flexible mode with the exact immutable release tool."""

    try:
        manifest = active_pointer(data_dir, release_root)
        if manifest is None:
            raise ReleaseError("the flexible pool has no active immutable release pointer")
        if explicit_exe is not None and explicit_exe.resolve(strict=True) != Path(
            str(manifest["appExe"])
        ).resolve(strict=True):
            raise ReleaseError("the explicitly requested executable is not the active immutable release")
        report = run_json(
            [
                str(manifest["poolAdminExe"]),
                "certify",
                "--db",
                str(db_path),
                "--full-integrity",
                "--require-review-ready",
            ],
            timeout=600,
        )
        errors = flexible_report_issues(report, pool, manifest)
    except (OSError, ReleaseError, ValueError) as error:
        errors = [f"flexible-pool certification cannot be proved: {error}"]
        report = {}
    payload = {
        "ok": not errors,
        "status": "READY" if not errors else "BLOCKED",
        "mode": "flexible-pool",
        "errors": errors,
        "evidence": report,
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if not errors else 1


def _canonical_query_rows(
    connection: sqlite3.Connection,
    sql: str,
    parameters: tuple[object, ...] = (),
) -> dict[str, object]:
    cursor = connection.execute(sql, parameters)
    columns = [str(column[0]) for column in cursor.description or ()]
    rows: list[list[object]] = []
    for row in cursor.fetchall():
        values: list[object] = []
        for value in tuple(row):
            if isinstance(value, bytes):
                values.append({"sqliteBlobHex": value.hex()})
            else:
                values.append(value)
        rows.append(values)
    return {"columns": columns, "rows": rows}


def canonical_evidence_manifest(
    connection: sqlite3.Connection,
    baseline: int,
) -> tuple[str, dict[str, object]]:
    """Hash canonical ordered raw evidence so the certificate names one exact history."""
    event_ids = [
        int(row[0])
        for row in connection.execute(
            """SELECT id FROM review_events
                WHERE id > ? AND source IN ('couch', 'couch_spot_check') ORDER BY id""",
            (baseline,),
        )
    ]
    event_placeholders = ",".join("?" for _ in event_ids) or "NULL"
    event_parameters: tuple[object, ...] = tuple(event_ids)
    original_entry_ids = [
        str(row[0])
        for row in connection.execute(
            f"""SELECT entry_id FROM review_compensation_ledger
                  WHERE review_event_id IN ({event_placeholders})
                    AND reverses_entry_id IS NULL
                  ORDER BY id""",
            event_parameters,
        )
    ]
    entry_placeholders = ",".join("?" for _ in original_entry_ids) or "NULL"
    entry_parameters: tuple[object, ...] = tuple(original_entry_ids)
    effect_ids = [
        int(row[0])
        for row in connection.execute(
            f"""SELECT id FROM human_decision_effect_events
                  WHERE review_event_id IN ({event_placeholders}) ORDER BY id""",
            event_parameters,
        )
    ]
    effect_placeholders = ",".join("?" for _ in effect_ids) or "NULL"
    effect_parameters: tuple[object, ...] = tuple(effect_ids)
    receipt_ids = [
        int(row[0])
        for row in connection.execute(
            f"""SELECT DISTINCT p.id
                   FROM playback_receipts p
                   JOIN review_compensation_ledger l
                     ON l.review_event_id IN ({event_placeholders})
                    AND l.reverses_entry_id IS NULL
                    AND l.segment_id = p.segment_id
                    AND l.reviewer = p.reviewer COLLATE NOCASE
                    AND p.segment_revision = CASE
                          WHEN l.source = 'couch' THEN l.decision_revision - 1
                          ELSE l.decision_revision
                        END
                    AND p.policy_version = ?
                  ORDER BY p.id""",
            (*event_parameters, PLAYBACK_POLICY_VERSION),
        )
    ]
    receipt_placeholders = ",".join("?" for _ in receipt_ids) or "NULL"
    receipt_parameters: tuple[object, ...] = tuple(receipt_ids)

    tables = {
        "review_effect_state": _canonical_query_rows(
            connection,
            "SELECT * FROM review_effect_state WHERE singleton_key=1 ORDER BY singleton_key",
        ),
        "review_events": _canonical_query_rows(
            connection,
            f"SELECT * FROM review_events WHERE id IN ({event_placeholders}) ORDER BY id",
            event_parameters,
        ),
        "speech_segments": _canonical_query_rows(
            connection,
            f"""SELECT s.* FROM speech_segments s
                 WHERE s.id IN (
                    SELECT segment_id FROM review_events WHERE id IN ({event_placeholders})
                 )
                 ORDER BY s.id""",
            event_parameters,
        ),
        "review_compensation_originals": _canonical_query_rows(
            connection,
            f"""SELECT * FROM review_compensation_ledger
                 WHERE review_event_id IN ({event_placeholders})
                   AND reverses_entry_id IS NULL ORDER BY id""",
            event_parameters,
        ),
        "review_compensation_reversals": _canonical_query_rows(
            connection,
            f"""SELECT * FROM review_compensation_ledger
                 WHERE reverses_entry_id IN ({entry_placeholders}) ORDER BY id""",
            entry_parameters,
        ),
        "playback_receipts": _canonical_query_rows(
            connection,
            f"SELECT * FROM playback_receipts WHERE id IN ({receipt_placeholders}) ORDER BY id",
            receipt_parameters,
        ),
        "human_decision_effect_events": _canonical_query_rows(
            connection,
            f"""SELECT * FROM human_decision_effect_events
                 WHERE id IN ({effect_placeholders}) ORDER BY id""",
            effect_parameters,
        ),
        "human_decision_effect_reversals": _canonical_query_rows(
            connection,
            f"""SELECT * FROM human_decision_effect_reversals
                 WHERE effect_event_id IN ({effect_placeholders}) ORDER BY effect_event_id""",
            effect_parameters,
        ),
        "review_pilot_hidden_keys": _canonical_query_rows(
            connection,
            """SELECT * FROM review_pilot_hidden_keys
                WHERE after_review_event_id = ?
                ORDER BY policy_sha256, after_review_event_id, reviewer COLLATE NOCASE, segment_id""",
            (baseline,),
        ),
        "spot_checks": _canonical_query_rows(
            connection,
            f"""SELECT s.* FROM spot_checks s
                 WHERE EXISTS (
                    SELECT 1 FROM review_events e
                     WHERE e.id IN ({event_placeholders})
                       AND e.source = 'couch_spot_check'
                       AND e.segment_id = s.segment_id
                       AND e.reviewer = s.reviewer COLLATE NOCASE
                 )
                 ORDER BY s.segment_id, s.reviewer COLLATE NOCASE""",
            event_parameters,
        ),
        "effective_review_events_v60": _canonical_query_rows(
            connection,
            """SELECT * FROM effective_review_events_v60
                WHERE review_event_id > ? AND policy_version = ?
                  AND source IN ('couch', 'couch_spot_check')
                ORDER BY review_event_id""",
            (baseline, POLICY_VERSION),
        ),
    }
    manifest: dict[str, object] = {
        "manifestVersion": 1,
        "schemaVersion": REQUIRED_SCHEMA,
        "policyVersion": POLICY_VERSION,
        "afterReviewEventId": baseline,
        "tables": tables,
    }
    canonical_json = json.dumps(
        manifest,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(canonical_json).hexdigest(), manifest


def read_events(
    connection: sqlite3.Connection, policy: ReviewPilotPolicy
) -> list[ReviewEventEvidence]:
    """Return effective non-skips plus every permanent raw skip safety slot."""
    history = audit_pilot_review_history(connection, policy)
    return [
        ReviewEventEvidence(
            event_id=event.event_id,
            segment_id=event.segment_id,
            reviewer=event.reviewer,
            action=event.action,
            source=event.source,
            created_at=event.created_at,
            timestamp_ms=event.timestamp_ms,
            duration_ms=event.duration_ms,
            compensation_action=event.compensation_action,
            operation_id=event.operation_id,
            operation_payload_hash=event.operation_payload_hash,
            app_git_sha=event.app_git_sha,
            playback_guard_version=event.playback_guard_version,
            ledger_id=event.ledger_id,
            ledger_entry_id=event.ledger_entry_id,
            canonical_work_id=event.canonical_work_id,
            canonical_identity_kind=event.canonical_identity_kind,
            decision_revision=event.decision_revision,
        )
        for event in history.effective_events
    ]


def raw_event_provenance_issues(
    connection: sqlite3.Connection,
    baseline: int,
    expected_app_git_sha: object,
) -> tuple[int, list[str]]:
    """Bind every raw pilot row, including reversed history, to one certified build/guard."""
    rows = connection.execute(
        """SELECT id, app_git_sha, playback_guard_version FROM review_events
            WHERE id > ? AND source IN ('couch', 'couch_spot_check') ORDER BY id""",
        (baseline,),
    ).fetchall()
    errors: list[str] = []
    for event_id, app_git_sha, guard in rows:
        if (
            not isinstance(app_git_sha, str)
            or len(app_git_sha) != 40
            or any(character not in "0123456789abcdef" for character in app_git_sha)
        ):
            errors.append(f"raw paid event {event_id} app_git_sha is not exact 40-lowerhex")
        elif app_git_sha != expected_app_git_sha:
            errors.append(
                f"raw paid event {event_id} build {app_git_sha} differs from certified executable "
                f"build {expected_app_git_sha}"
            )
        if guard != PLAYBACK_GUARD_VERSION:
            errors.append(
                f"raw paid event {event_id} playback guard {guard!r}; required {PLAYBACK_GUARD_VERSION!r}"
            )
    return len(rows), errors


def _learning_text_key(value: str) -> str:
    return " ".join(unicodedata.normalize("NFC", value.strip()).lower().split())


def _current_human_answer_key(connection: sqlite3.Connection, segment_id: str) -> str | None:
    row = connection.execute(
        """SELECT raw_transcript, annotated_transcript, verdict_transcript, human_decision, verdict
             FROM speech_segments WHERE id = ?""",
        (segment_id,),
    ).fetchone()
    if row is None:
        return None
    raw, annotated, verdict_transcript, human_decision, verdict = row
    human = human_decision.casefold() if isinstance(human_decision, str) else ""
    final = verdict.casefold() if isinstance(verdict, str) else ""
    if human in {"reject", "human_reject"} or final == "human_reject":
        return None
    if human not in {"accept", "edit", "human_accept", "human_edit"} and final not in {
        "human_accept",
        "human_edit",
    }:
        return None
    for candidate in (verdict_transcript, annotated, raw):
        if isinstance(candidate, str) and candidate.strip():
            return _learning_text_key(candidate)
    return None


def read_hidden_quality(
    connection: sqlite3.Connection,
    state: HiddenPilotState,
) -> tuple[dict[tuple[str, str], tuple[int, float]], list[str]]:
    quality: dict[tuple[str, str], tuple[int, float]] = {}
    errors: list[str] = []
    for reviewer, segment_ids in state.grants.items():
        for segment_id in sorted(segment_ids):
            rows = connection.execute(
                """SELECT action, submitted_transcript, expected_transcript, noticed, cer FROM spot_checks
                    WHERE segment_id = ? AND reviewer = ? COLLATE NOCASE""",
                (segment_id, reviewer),
            ).fetchall()
            if len(rows) != 1:
                errors.append(
                    f"{reviewer}/{segment_id} has {len(rows)} hidden result rows; exactly one is required"
                )
                continue
            action, submitted, expected, noticed, cer = rows[0]
            if action not in {"accept", "edit"}:
                errors.append(f"{reviewer}/{segment_id} hidden action {action!r} is not a successful judgement")
                continue
            if not isinstance(submitted, str) or not isinstance(expected, str):
                errors.append(f"{reviewer}/{segment_id} has non-text hidden transcripts")
                continue
            submitted_key = _learning_text_key(submitted)
            expected_key = _learning_text_key(expected)
            if not submitted_key or submitted_key != expected_key:
                errors.append(f"{reviewer}/{segment_id} stored hidden transcripts do not derive a correct answer")
                continue
            current_key = _current_human_answer_key(connection, segment_id)
            if current_key is None or expected_key != current_key:
                errors.append(f"{reviewer}/{segment_id} stored expected transcript is not the current human answer")
                continue
            if type(noticed) is not int or noticed != 1:
                errors.append(f"{reviewer}/{segment_id} noticed={noticed!r}; required exact INTEGER 1")
                continue
            if type(cer) not in (int, float) or not math.isfinite(float(cer)) or float(cer) != 0.0:
                errors.append(f"{reviewer}/{segment_id} CER={cer!r}; required exact numeric 0")
                continue
            quality[(reviewer, segment_id)] = (noticed, float(cer))
    return quality, errors


def final_event_playback_issue(
    connection: sqlite3.Connection,
    event: ReviewEventEvidence,
) -> str | None:
    """Prove the exact receipt revision authorized one immutable pilot event.

    SQLite's ``created_at`` has one-second resolution, so timestamp ordering cannot distinguish a
    receipt minted immediately before an event from one inserted immediately after it. The
    compensation ledger is committed atomically with every event and records the decision revision:
    corpus decisions increment the row once (receipt revision = decision_revision - 1), while hidden
    checks do not mutate it (receipt revision = decision_revision). This is the fixed schema-60
    canary's exact, non-heuristic binding. Policy 3 additionally requires the receipt's integer
    source window to equal the server-owned alignment span, not merely have the same duration.
    """
    ledger_rows = connection.execute(
        """SELECT policy_version, decision_revision, duration_ms, reviewer, segment_id, source,
                  canonical_work_id, canonical_identity_kind
             FROM review_compensation_ledger
            WHERE review_event_id = ?""",
        (event.event_id,),
    ).fetchall()
    if len(ledger_rows) != 1:
        return f"event has {len(ledger_rows)} compensation ledger rows; required exactly one"
    (
        ledger_policy,
        decision_revision,
        ledger_duration,
        ledger_reviewer,
        ledger_segment,
        ledger_source,
        ledger_work_id,
        ledger_identity_kind,
    ) = ledger_rows[0]
    if ledger_policy != POLICY_VERSION:
        return f"event compensation policy is {ledger_policy!r}; required {POLICY_VERSION!r}"
    if (
        ledger_segment != event.segment_id
        or ledger_source != event.source
        or not isinstance(ledger_reviewer, str)
        or ledger_reviewer.casefold() != event.reviewer.casefold()
    ):
        return "event and compensation ledger identity disagree"
    if type(decision_revision) is not int or decision_revision < 0:
        return f"ledger decision_revision={decision_revision!r} is not a non-negative integer"
    required_revision = decision_revision - 1 if event.source == "couch" else decision_revision
    if required_revision < 0:
        return f"event source {event.source!r} implies impossible receipt revision {required_revision}"
    if type(ledger_duration) is not int or ledger_duration <= 0:
        return f"ledger duration {ledger_duration!r}ms is not a positive integer"
    if type(event.timestamp_ms) is not int or event.timestamp_ms <= 0:
        return f"event timestamp_ms={event.timestamp_ms!r} is not a positive integer"

    identity = connection.execute(
        """SELECT CAST(NULLIF(TRIM(COALESCE(audio_content_hash, '')), '') AS TEXT),
                   duration_ms, review_revision, alignment_json
             FROM speech_segments WHERE id = ?""",
        (event.segment_id,),
    ).fetchone()
    if identity is None:
        return "segment is missing, so its certified audio identity cannot be proved"
    content_hash, current_duration, current_revision, alignment_json = identity
    if not isinstance(content_hash, str) or not is_canonical_audio_content_hash(content_hash):
        return "server-owned audio content hash is not canonical lowercase 64-hex"
    if type(current_revision) is not int or current_revision != decision_revision:
        return (
            f"current review revision {current_revision!r} disagrees with immutable "
            f"decision revision {decision_revision}"
        )
    if type(current_duration) is not int or current_duration != ledger_duration:
        return (
            f"current duration {current_duration!r}ms disagrees with immutable "
            f"event duration {ledger_duration}ms"
        )
    expected_work_id, work_reason = canonical_reviewer_work_id(
        event.reviewer, content_hash, alignment_json
    )
    if (
        expected_work_id is None
        or ledger_identity_kind != CANONICAL_IDENTITY_KIND
        or ledger_work_id != expected_work_id
    ):
        return (
            "compensation work identity is not the exact content-hash/source-span identity: "
            f"expected={expected_work_id!r}, reason={work_reason or 'none'}"
        )
    expected_source_span, span_reason = canonical_source_span(alignment_json)
    if expected_source_span is None:
        return f"server-owned playback source span is invalid: {span_reason}"
    duration_issue = source_span_duration_issue(
        ledger_duration,
        expected_source_span,
        subject="immutable event duration",
    )
    if duration_issue:
        return f"server-owned playback source span is invalid: {duration_issue}"

    candidates = connection.execute(
        """SELECT id, played_ms, clip_duration_ms, coverage_ratio, policy_version, started_at_ms,
                  source_start_ms, source_end_ms
             FROM playback_receipts
            WHERE segment_id = ?
              AND reviewer = ? COLLATE NOCASE
              AND segment_revision = ?
              AND audio_fingerprint = ?
              AND typeof(policy_version) = 'integer'
              AND policy_version = ?""",
        (
            event.segment_id,
            event.reviewer,
            required_revision,
            content_hash,
            PLAYBACK_POLICY_VERSION,
        ),
    ).fetchall()
    valid: list[float] = []
    invalid: list[str] = []
    for candidate in candidates:
        receipt = candidate[:5]
        started_at_ms = candidate[5]
        source_start_ms = candidate[6]
        source_end_ms = candidate[7]
        if type(started_at_ms) is not int or started_at_ms < 0:
            invalid.append(f"receipt {receipt[0]} started_at_ms is not a non-negative integer")
            continue
        if started_at_ms > event.timestamp_ms:
            invalid.append(
                f"receipt {receipt[0]} started at {started_at_ms} after event {event.event_id} "
                f"at {event.timestamp_ms}"
            )
            continue
        span_issue = receipt_source_span_issue(
            receipt[0], source_start_ms, source_end_ms, expected_source_span
        )
        if span_issue:
            invalid.append(span_issue)
            continue
        coverage, reason = canonical_receipt_coverage(receipt, expected_duration_ms=ledger_duration)
        if reason:
            invalid.append(reason)
        elif coverage is not None:
            valid.append(coverage)
    best = max(valid, default=0.0)
    if best < MIN_PLAYBACK_COVERAGE:
        detail = f"; invalid evidence: {invalid[0]}" if invalid else ""
        return (
            f"receipt revision {required_revision} best canonical coverage {best:.2f} "
            f"< {MIN_PLAYBACK_COVERAGE:.2f}{detail}"
        )
    return None


def _canonical_reviewer(policy: ReviewPilotPolicy, actual: str) -> str | None:
    matches = [name for name in policy.reviewer_caps if name.casefold() == actual.strip().casefold()]
    return matches[0] if len(matches) == 1 else None


def compensation_issues(
    report: dict[str, Any],
    *,
    label: str,
    policy: ReviewPilotPolicy,
    state: HiddenPilotState,
    focus: PilotFocusEvidence,
) -> list[str]:
    errors: list[str] = []
    if report.get("ok") is not True:
        details = report.get("errors")
        errors.append(f"{label} compensation audit failed: {details!r}")
        return errors
    expected: dict[str, object] = {
        "schemaVersion": REQUIRED_SCHEMA,
        "policyVersion": POLICY_VERSION,
        "effectiveAfterEventId": policy.after_review_event_id,
        "pilotPolicySha256": state.policy_sha256,
        "pilotCorpusActions": TOTAL_CORPUS_ACTIONS,
        "pilotHiddenActions": TOTAL_HIDDEN_KEYS,
        "pilotUiActions": MAX_UI_ACTIONS,
        "pilotHiddenGrants": TOTAL_HIDDEN_KEYS,
        "pilotHiddenResolved": TOTAL_HIDDEN_KEYS,
        "pilotHiddenUnresolved": 0,
        "postCutoffEvents": MAX_UI_ACTIONS,
        "accountingEffectiveEvents": MAX_UI_ACTIONS,
        "ledgerEntries": MAX_UI_ACTIONS,
        "durableOperationReceipts": MAX_UI_ACTIONS,
        "fallbackLedgerEntries": 0,
        "compensationForeignKeyViolations": 0,
        "focusIds": focus.segment_id_count,
        "canonicalFocusRows": focus.segment_id_count,
        "uniqueCanonicalFocusWorkIds": focus.segment_id_count,
    }
    for field, wanted in expected.items():
        if report.get(field) != wanted:
            errors.append(f"{label} compensation {field}={report.get(field)!r}; required {wanted!r}")
    raw_events = report.get("rawPostCutoffEvents")
    raw_ledger = report.get("rawLedgerEntries")
    reversals = report.get("reversalEntries")
    if type(raw_events) is not int or type(raw_ledger) is not int or type(reversals) is not int:
        errors.append(f"{label} compensation lacks exact raw/reversal evidence counts")
    else:
        if raw_events != MAX_UI_ACTIONS + reversals:
            errors.append(
                f"{label} raw paid events {raw_events} are not 24 effective + {reversals} reversals"
            )
        if raw_ledger != MAX_UI_ACTIONS + 2 * reversals:
            errors.append(
                f"{label} raw ledger rows {raw_ledger} are not 24 originals/active + exact reversal pairs"
            )
    earned = report.get("totalEarnedMicroIqd")
    if type(earned) is not int or earned <= 0:
        errors.append(f"{label} compensation totalEarnedMicroIqd must be positive after 24 non-skip actions")
    return errors


def certification_issues(
    *,
    policy: ReviewPilotPolicy,
    state: HiddenPilotState,
    events: list[ReviewEventEvidence],
    hidden_quality: dict[tuple[str, str], tuple[int, float]],
    playback_failures: list[str],
    playback_checked: int,
    compensation_reports: list[tuple[str, dict[str, Any]]],
    focus: PilotFocusEvidence,
    focus_ids: set[str],
    expected_app_git_sha: str | None = None,
) -> list[str]:
    errors: list[str] = []
    expected_roster = set(PILOT_REVIEWERS)
    if set(policy.reviewer_caps) != expected_roster:
        errors.append(f"pilot roster is {sorted(policy.reviewer_caps)}; required {sorted(expected_roster)}")
    if policy.max_total_corpus_actions != TOTAL_CORPUS_ACTIONS:
        errors.append(f"pilot corpus cap is {policy.max_total_corpus_actions}; required {TOTAL_CORPUS_ACTIONS}")

    canonical_events: list[tuple[ReviewEventEvidence, str]] = []
    observed_builds: set[str] = set()
    for event in events:
        reviewer = _canonical_reviewer(policy, event.reviewer)
        if reviewer is None:
            errors.append(f"event {event.event_id} has unauthorized reviewer {event.reviewer!r}")
            continue
        canonical_events.append((event, reviewer))
        if event.source not in ALLOWED_SOURCES:
            errors.append(f"event {event.event_id} has non-pilot source {event.source!r}")
        if event.action == "skip":
            errors.append(f"event {event.event_id} is a skip; certification permits zero skips")
        elif event.action not in ALLOWED_ACTIONS:
            errors.append(f"event {event.event_id} has unsupported action {event.action!r}")
        if not event.created_at:
            errors.append(f"event {event.event_id} has no durable creation time")
        if type(event.timestamp_ms) is not int or event.timestamp_ms <= 0:
            errors.append(f"event {event.event_id} has invalid timestamp_ms={event.timestamp_ms!r}")
        if event.segment_id not in focus_ids:
            errors.append(
                f"event {event.event_id} segment {event.segment_id!r} is outside the exact active focus"
            )
        if (
            len(event.app_git_sha) != 40
            or any(character not in "0123456789abcdef" for character in event.app_git_sha)
        ):
            errors.append(f"event {event.event_id} app_git_sha is not exact 40-lowerhex")
        else:
            observed_builds.add(event.app_git_sha)
            if expected_app_git_sha is not None and event.app_git_sha != expected_app_git_sha:
                errors.append(
                    f"event {event.event_id} build {event.app_git_sha} differs from certified executable "
                    f"build {expected_app_git_sha}"
                )
        if event.playback_guard_version != PLAYBACK_GUARD_VERSION:
            errors.append(
                f"event {event.event_id} playback guard {event.playback_guard_version!r}; "
                f"required {PLAYBACK_GUARD_VERSION!r}"
            )
        try:
            canonical_operation = str(uuid.UUID(event.operation_id))
        except (ValueError, AttributeError):
            canonical_operation = ""
        if canonical_operation != event.operation_id:
            errors.append(f"event {event.event_id} has no canonical operation UUID")
        if (
            len(event.operation_payload_hash) != 64
            or any(character not in "0123456789abcdef" for character in event.operation_payload_hash)
        ):
            errors.append(f"event {event.event_id} operation payload hash is not lowercase SHA-256")
        if event.compensation_action != event.action:
            errors.append(
                f"event {event.event_id} compensation action {event.compensation_action!r} "
                f"differs from effective action {event.action!r}"
            )
        if event.canonical_identity_kind != CANONICAL_IDENTITY_KIND:
            errors.append(f"event {event.event_id} uses a noncanonical compensation identity kind")

    if len(observed_builds) > 1:
        errors.append(f"effective pilot events span {len(observed_builds)} producing builds")

    if len(events) != MAX_UI_ACTIONS:
        errors.append(f"post-baseline history has {len(events)}/{MAX_UI_ACTIONS} required UI actions")
    for reviewer in PILOT_REVIEWERS:
        corpus = [event for event, who in canonical_events if who == reviewer and event.source == "couch"]
        hidden = [
            event for event, who in canonical_events if who == reviewer and event.source == "couch_spot_check"
        ]
        if len(corpus) != CORPUS_ACTIONS_PER_REVIEWER:
            errors.append(f"{reviewer} has {len(corpus)}/{CORPUS_ACTIONS_PER_REVIEWER} corpus actions")
        if len({event.segment_id for event in corpus}) != len(corpus):
            errors.append(f"{reviewer} corpus sample repeats a segment and is not ten distinct decisions")
        if len(hidden) != HIDDEN_KEYS_PER_REVIEWER:
            errors.append(f"{reviewer} has {len(hidden)}/{HIDDEN_KEYS_PER_REVIEWER} hidden actions")

        if state.corpus_actions.get(reviewer) != CORPUS_ACTIONS_PER_REVIEWER:
            errors.append(
                f"{reviewer} durable corpus counter is {state.corpus_actions.get(reviewer)}/"
                f"{CORPUS_ACTIONS_PER_REVIEWER}"
            )
        if state.hidden_actions.get(reviewer) != HIDDEN_KEYS_PER_REVIEWER:
            errors.append(
                f"{reviewer} durable hidden counter is {state.hidden_actions.get(reviewer)}/"
                f"{HIDDEN_KEYS_PER_REVIEWER}"
            )
        if len(state.grants.get(reviewer, set())) != HIDDEN_KEYS_PER_REVIEWER:
            errors.append(f"{reviewer} has {len(state.grants.get(reviewer, set()))}/2 durable hidden grants")
        outside_focus = sorted(state.grants.get(reviewer, set()) - focus_ids)
        if outside_focus:
            errors.append(
                f"{reviewer} hidden grant {outside_focus[0]!r} is outside the exact active focus"
            )
        if len(state.completed_keys.get(reviewer, set())) != HIDDEN_KEYS_PER_REVIEWER:
            errors.append(f"{reviewer} has {len(state.completed_keys.get(reviewer, set()))}/2 completed hidden keys")
        if state.skipped_keys.get(reviewer):
            errors.append(f"{reviewer} skipped {len(state.skipped_keys[reviewer])} hidden key(s)")
        if state.unresolved_keys.get(reviewer):
            errors.append(f"{reviewer} has {len(state.unresolved_keys[reviewer])} unresolved hidden key(s)")
        for segment_id in sorted(state.grants.get(reviewer, set())):
            result = hidden_quality.get((reviewer, segment_id))
            if result is None:
                errors.append(f"{reviewer}/{segment_id} has no auditable hidden result")
                continue
            noticed, cer = result
            if type(noticed) is not int or noticed != 1 or type(cer) not in (int, float) or not math.isfinite(float(cer)) or float(cer) != 0.0:
                errors.append(
                    f"{reviewer}/{segment_id} hidden result is noticed={noticed}, CER={cer}; required 1 and 0"
                )

    if state.total_corpus_actions != TOTAL_CORPUS_ACTIONS:
        errors.append(f"durable corpus total is {state.total_corpus_actions}/{TOTAL_CORPUS_ACTIONS}")
    if state.total_hidden_actions != TOTAL_HIDDEN_KEYS:
        errors.append(f"durable hidden total is {state.total_hidden_actions}/{TOTAL_HIDDEN_KEYS}")
    if state.total_ui_actions != MAX_UI_ACTIONS:
        errors.append(f"durable UI total is {state.total_ui_actions}/{MAX_UI_ACTIONS}")
    if playback_checked != MAX_UI_ACTIONS:
        errors.append(f"playback audit covered {playback_checked}/{MAX_UI_ACTIONS} required decisions")
    errors.extend(playback_failures)
    for label, report in compensation_reports:
        errors.extend(compensation_issues(report, label=label, policy=policy, state=state, focus=focus))
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    parser.add_argument("--release-root", type=Path)
    raw_args = list(argv) if argv is not None else sys.argv[1:]
    args = parser.parse_args(argv)

    try:
        data_dir = args.data_dir or default_data_dir()
    except RuntimeError as error:
        print(json.dumps({"ok": False, "status": "BLOCKED", "errors": [str(error)]}, indent=2))
        return 1
    db_path = data_dir / "cortex-speech.db"
    focus_path = data_dir / "voice_focus.json"
    errors: list[str] = []

    if not db_path.is_file():
        print(
            json.dumps(
                {"ok": False, "status": "BLOCKED", "errors": [f"database not found: {db_path}"]},
                indent=2,
            )
        )
        return 1
    try:
        connection = connect_read_only(db_path)
        try:
            pool = active_flexible_pool_identity(connection)
        finally:
            connection.close()
    except (OSError, sqlite3.Error, PolicyBroken) as error:
        print(
            json.dumps(
                {"ok": False, "status": "BLOCKED", "errors": [f"active review mode cannot be proved: {error}"]},
                indent=2,
            )
        )
        return 1
    if pool is not None:
        if (data_dir / "review_pilot_policy.json").exists():
            print(
                json.dumps(
                    {
                        "ok": False,
                        "status": "BLOCKED",
                        "mode": "conflicting",
                        "errors": ["flexible pool and legacy controlled-pilot policy are active together"],
                    },
                    indent=2,
                )
            )
            return 1
        try:
            release_root = args.release_root or default_release_root()
        except RuntimeError as error:
            print(json.dumps({"ok": False, "status": "BLOCKED", "errors": [str(error)]}, indent=2))
            return 1
        explicit_exe = args.exe if any(value == "--exe" or value.startswith("--exe=") for value in raw_args) else None
        return certify_flexible_pool(data_dir, db_path, release_root, pool, explicit_exe=explicit_exe)

    evidence: dict[str, Any] = {
        "database": str(db_path.resolve()),
        "focus": str(focus_path.resolve()),
        "executable": str(args.exe.resolve()),
    }

    if not focus_path.is_file():
        errors.append(f"focus not found: {focus_path}")
    if not args.exe.is_file():
        errors.append(f"release executable not found: {args.exe}")
    if errors:
        print(json.dumps({"ok": False, "status": "BLOCKED", "errors": errors, "evidence": evidence}, indent=2))
        return 1

    try:
        focus_before = verify_controlled_pilot_focus(data_dir)
        focus_ids_before = load_voice_focus_ids(data_dir)
        policy_before = read_policy(data_dir / "review_pilot_policy.json")
        policy_digest = policy_sha256(policy_before)
        executable_sha_before = sha256_file(args.exe)
        baked_git_sha_before = extract_baked_sha(args.exe.read_bytes())
        can_enforce, binary_reason = binary_can_warn(args.exe)
        compensation_before = audit_compensation(db_path, focus_path)
    except (OSError, PilotContractError, PilotFocusError, sqlite3.Error, ValueError) as error:
        print(
            json.dumps(
                {"ok": False, "status": "BLOCKED", "errors": [f"preflight cannot be proved: {error}"], "evidence": evidence},
                indent=2,
            )
        )
        return 1

    if not can_enforce:
        errors.append(binary_reason)
    if (
        not isinstance(baked_git_sha_before, str)
        or len(baked_git_sha_before) != 40
        or any(character not in "0123456789abcdef" for character in baked_git_sha_before)
    ):
        errors.append("release executable lacks one exact 40-lowerhex CORTEX_BUILD_SHA marker")

    connection: sqlite3.Connection | None = None
    receipt_count = 0
    try:
        connection = connect_read_only(db_path)
        state = audit_active_hidden_state(connection, data_dir, db_path, policy_before)
        events = read_events(connection, policy_before)
        raw_provenance_checked, provenance_errors = raw_event_provenance_issues(
            connection,
            policy_before.after_review_event_id,
            baked_git_sha_before,
        )
        errors.extend(provenance_errors)
        manifest_sha_before, _manifest_before = canonical_evidence_manifest(
            connection, policy_before.after_review_event_id
        )
        hidden_quality, quality_errors = read_hidden_quality(connection, state)
        errors.extend(quality_errors)
        receipt_count, receipt_semantic_errors = playback_receipt_semantic_issues(connection)
        errors.extend(f"playback receipt integrity: {reason}" for reason in receipt_semantic_errors)
        playback_failures: list[str] = []
        playback_checked = 0
        for event in events:
            if event.source not in ALLOWED_SOURCES or event.action == "skip":
                continue
            playback_checked += 1
            reason = final_event_playback_issue(connection, event)
            if reason:
                playback_failures.append(
                    f"event {event.event_id} ({event.source}, {event.reviewer}) lacks exact playback evidence: {reason}"
                )
        max_event_id = int(connection.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0])
    except (PilotContractError, sqlite3.Error, TypeError, ValueError) as error:
        errors.append(f"durable canary snapshot cannot be proved: {error}")
        state = None
        events = []
        hidden_quality = {}
        playback_failures = []
        playback_checked = 0
        max_event_id = -1
        manifest_sha_before = ""
        raw_provenance_checked = 0
    finally:
        if connection is not None:
            connection.rollback()
            connection.close()

    try:
        focus_after = verify_controlled_pilot_focus(data_dir)
        focus_ids_after = load_voice_focus_ids(data_dir)
        policy_after = read_policy(data_dir / "review_pilot_policy.json")
        executable_sha_after = sha256_file(args.exe)
        baked_git_sha_after = extract_baked_sha(args.exe.read_bytes())
        compensation_after = audit_compensation(db_path, focus_path)
        manifest_connection = connect_read_only(db_path)
        try:
            manifest_sha_after, _manifest_after = canonical_evidence_manifest(
                manifest_connection, policy_before.after_review_event_id
            )
        finally:
            manifest_connection.rollback()
            manifest_connection.close()
    except (OSError, PilotContractError, PilotFocusError, sqlite3.Error, ValueError) as error:
        errors.append(f"postflight cannot be proved: {error}")
        focus_after = focus_before
        focus_ids_after = set()
        policy_after = policy_before
        executable_sha_after = ""
        baked_git_sha_after = None
        compensation_after = {"ok": False, "errors": ["postflight failed"]}
        manifest_sha_after = ""

    if focus_after != focus_before:
        errors.append("controlled focus changed during certification")
    if focus_ids_after != focus_ids_before:
        errors.append("controlled focus membership changed during certification")
    if policy_after != policy_before or policy_sha256(policy_after) != policy_digest:
        errors.append("controlled policy changed during certification")
    if executable_sha_after != executable_sha_before:
        errors.append("release executable changed during certification")
    if baked_git_sha_after != baked_git_sha_before:
        errors.append("release executable build marker changed during certification")
    if manifest_sha_after != manifest_sha_before:
        errors.append("raw certification evidence changed during certification")

    evidence.update(
        {
            "schemaVersion": compensation_after.get("schemaVersion"),
            "policySha256": policy_digest,
            "afterReviewEventId": policy_before.after_review_event_id,
            "maxReviewEventId": max_event_id,
            "focusIds": focus_before.segment_id_count,
            "focusSha256": focus_before.sorted_unique_segment_ids_sha256,
            "playbackReceiptsAudited": receipt_count,
            "executableSha256": executable_sha_before,
            "appGitSha": baked_git_sha_before,
            "evidenceManifestSha256": manifest_sha_before,
            "binaryPlaybackGuard": can_enforce,
            "corpusActions": state.total_corpus_actions if state is not None else None,
            "hiddenActions": state.total_hidden_actions if state is not None else None,
            "uiActions": state.total_ui_actions if state is not None else None,
            "hiddenGrants": sum(len(ids) for ids in state.grants.values()) if state is not None else None,
            "hiddenCompleted": sum(len(ids) for ids in state.completed_keys.values()) if state is not None else None,
            "playbackChecked": playback_checked,
            "rawPaidEventProvenanceChecked": raw_provenance_checked,
            "postCutoffEvents": compensation_after.get("postCutoffEvents"),
            "accountingEffectiveEvents": compensation_after.get("accountingEffectiveEvents"),
            "rawPostCutoffEvents": compensation_after.get("rawPostCutoffEvents"),
            "ledgerEntries": compensation_after.get("ledgerEntries"),
            "rawLedgerEntries": compensation_after.get("rawLedgerEntries"),
            "reversalEntries": compensation_after.get("reversalEntries"),
            "durableOperationReceipts": compensation_after.get("durableOperationReceipts"),
            "totalEarnedMicroIqd": compensation_after.get("totalEarnedMicroIqd"),
        }
    )
    if state is not None:
        errors.extend(
            certification_issues(
                policy=policy_before,
                state=state,
                events=events,
                hidden_quality=hidden_quality,
                playback_failures=playback_failures,
                playback_checked=playback_checked,
                compensation_reports=[
                    ("preflight", compensation_before),
                    ("postflight", compensation_after),
                ],
                focus=focus_before,
                focus_ids=focus_ids_before,
                expected_app_git_sha=baked_git_sha_before,
            )
        )

    result = {
        "ok": not errors,
        "status": "CERTIFIED" if not errors else "NOT_CERTIFIED",
        "errors": errors,
        "evidence": evidence,
    }
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
