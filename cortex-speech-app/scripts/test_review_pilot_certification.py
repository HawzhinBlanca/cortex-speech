"""The final paid-review gate must distinguish ready capacity from completed evidence."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sqlite3
import sys
import tempfile
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("check_review_pilot_certification.py")
VERIFY_10 = SCRIPT.parents[2] / "scripts" / "verify_10.py"
sys.path.insert(0, str(SCRIPT.parent))

import check_review_pilot_certification as gate  # noqa: E402
from pilot_focus_contract import focus_evidence  # noqa: E402
from review_pilot_hidden_contract import HiddenPilotState, ReviewPilotPolicy, policy_sha256  # noqa: E402


PILOT_EVENT_IDS = {
    *{f"work-h-{index}" for index in range(10)},
    *{f"work-p-{index}" for index in range(10)},
    "hidden-h-1",
    "hidden-h-2",
    "hidden-p-1",
    "hidden-p-2",
}
FOCUS_IDS = PILOT_EVENT_IDS | {f"focus-filler-{index}" for index in range(8254)}
FOCUS = focus_evidence(FOCUS_IDS)
CONTENT_HASH = "a" * 64


def flexible_report() -> dict[str, object]:
    return {
        "reportSchema": 3,
        "readOnly": True,
        "generatedAtEpochSecs": 1_700_000_000,
        "appGitSha": "b" * 40,
        # Derived from the release contract, never a second hand-written number: the gate refuses
        # any schema other than release_private_production.EXPECTED_SCHEMA, and this fixture went
        # stale (65) the day the contract moved to 69.
        "databaseSchemaVersion": gate.EXPECTED_SCHEMA,
        "pool": {
            "poolId": "123e4567-e89b-42d3-a456-426614174000",
            "focusSegmentCount": 2,
            "focusSha256": "a" * 64,
            "reviewSegmentCount": 2,
            "excludedDuplicateCount": 0,
            "duplicateFamilyCount": 0,
            "dedupManifestSha256": "d" * 64,
            "championModelVersionId": "omniasr-7b-test",
            "championDeploymentSha256": "c" * 64,
        },
        "dedup": {
            "applied": True,
            "algorithmId": "cortex-cross-file-waveform-correlation-v1",
            "manifestSha256": "d" * 64,
            "sourceSegmentCount": 2,
            "canonicalSegmentCount": 2,
            "excludedSegmentCount": 0,
            "duplicateFamilyCount": 0,
            "unconfirmedRiskCount": 0,
        },
        "resolutionSummary": {
            "totalClips": 2,
            "resolvedClips": 0,
            "needsFirstOrSecondReview": 2,
            "needsThirdReview": 0,
            "ownerConflicts": 0,
        },
        "resolutionAuthority": {
            "consensusAgreements": 0,
            "ownerAdjudications": 0,
            "unresolvedConflicts": 0,
        },
        "coverageByVoice": [
            {
                "voiceName": "Lamo",
                "totalClips": 2,
                "zeroReviews": 2,
                "oneReview": 0,
                "twoReviews": 0,
                "threeOrMoreReviews": 0,
                "needsThirdReview": 0,
                "ownerConflicts": 0,
                "resolved": 0,
            }
        ],
        "voiceOutcomes": {
            "Lamo": {
                "total": 2,
                "retained": 0,
                "rejected": 0,
                "unresolved": 2,
                "consensusAgreements": 0,
                "ownerAdjudications": 0,
                "unresolvedConflicts": 0,
                "certificate": None,
            }
        },
        "reviewerVoiceTotals": [],
        "rights": {
            "allExact": True,
            "exactRows": 2,
            "segmentRows": 2,
            "conflictingRows": 0,
            "revokedRows": 0,
            "unstampedRows": 0,
        },
        "audio": {
            "allAvailable": True,
            "clips": 2,
            "missingClips": 0,
            "missingRecordings": 0,
        },
        "database": {
            "quickCheck": ["ok"],
            "fullIntegrityCheck": ["ok"],
            "foreignKeyViolations": 0,
            "healthy": True,
        },
        "disk": {"freeBytes": 50_000_000_000, "minimumFreeBytes": 20_000_000_000, "healthy": True},
        "snapshots": {
            "local": {"fresh": True, "verified": True, "ageSecs": 10, "targetRpoSecs": 600},
            "offsite": {"fresh": True, "verified": True, "ageSecs": 11, "targetRpoSecs": 600},
        },
        "gates": {
            "reviewReady": True,
            "duplicateExclusionsBound": True,
            "allClipsResolved": False,
            "rightsComplete": True,
            "everyVoiceCertified": False,
            "finalDatasetReady": False,
        },
    }


def flexible_manifest(root: Path) -> dict[str, object]:
    return {
        "appGitSha": "b" * 40,
        "appExe": str(root / "app.exe"),
        "poolAdminExe": str(root / "pool_admin.exe"),
        "dedupManifestSha256": "d" * 64,
    }


def policy() -> ReviewPilotPolicy:
    return ReviewPilotPolicy(863, 20, {"Rezan": 10, "Aram": 10})


def perfect_state() -> HiddenPilotState:
    grants = {
        "Rezan": {"hidden-h-1", "hidden-h-2"},
        "Aram": {"hidden-p-1", "hidden-p-2"},
    }
    return HiddenPilotState(
        policy_sha256=policy_sha256(policy()),
        grants={name: set(ids) for name, ids in grants.items()},
        session_keys={name: set(ids) for name, ids in grants.items()},
        completed_keys={name: set(ids) for name, ids in grants.items()},
        skipped_keys={"Rezan": set(), "Aram": set()},
        unresolved_keys={"Rezan": set(), "Aram": set()},
        corpus_actions={"Rezan": 10, "Aram": 10},
        hidden_actions={"Rezan": 2, "Aram": 2},
    )


def perfect_events() -> list[gate.ReviewEventEvidence]:
    events: list[gate.ReviewEventEvidence] = []
    event_id = 864
    for reviewer, prefix in (("Rezan", "h"), ("Aram", "p")):
        for index in range(10):
            events.append(
                gate.ReviewEventEvidence(
                    event_id,
                    f"work-{prefix}-{index}",
                    reviewer,
                    "accept",
                    "couch",
                    "2026-08-22 07:00:00",
                    1_700_000_000_000 + event_id,
                )
            )
            event_id += 1
        for index in range(2):
            events.append(
                gate.ReviewEventEvidence(
                    event_id,
                    f"hidden-{prefix}-{index + 1}",
                    reviewer,
                    "accept",
                    "couch_spot_check",
                    "2026-08-22 07:00:00",
                    1_700_000_000_000 + event_id,
                )
            )
            event_id += 1
    return events


def perfect_quality() -> dict[tuple[str, str], tuple[int, float]]:
    return {
        (reviewer, segment_id): (1, 0.0)
        for reviewer, segment_ids in perfect_state().grants.items()
        for segment_id in segment_ids
    }


def perfect_compensation() -> dict[str, object]:
    return {
        "ok": True,
        "errors": [],
        "schemaVersion": 60,
        "policyVersion": "review-iqd-v1-2026-08-21",
        "effectiveAfterEventId": 863,
        "pilotPolicySha256": policy_sha256(policy()),
        "pilotCorpusActions": 20,
        "pilotHiddenActions": 4,
        "pilotUiActions": 24,
        "pilotHiddenGrants": 4,
        "pilotHiddenResolved": 4,
        "pilotHiddenUnresolved": 0,
        "postCutoffEvents": 24,
        "accountingEffectiveEvents": 24,
        "rawPostCutoffEvents": 24,
        "ledgerEntries": 24,
        "rawLedgerEntries": 24,
        "reversalEntries": 0,
        "durableOperationReceipts": 24,
        "fallbackLedgerEntries": 0,
        "compensationForeignKeyViolations": 0,
        "focusIds": 8278,
        "canonicalFocusRows": 8278,
        "uniqueCanonicalFocusWorkIds": 8278,
        "totalEarnedMicroIqd": 1,
    }


def issues(
    *,
    state: HiddenPilotState | None = None,
    events: list[gate.ReviewEventEvidence] | None = None,
    quality: dict[tuple[str, str], tuple[int, float]] | None = None,
    playback_failures: list[str] | None = None,
    playback_checked: int = 24,
    compensation: dict[str, object] | None = None,
    expected_app_git_sha: str | None = None,
) -> list[str]:
    report = compensation or perfect_compensation()
    return gate.certification_issues(
        policy=policy(),
        state=state or perfect_state(),
        events=events if events is not None else perfect_events(),
        hidden_quality=quality if quality is not None else perfect_quality(),
        playback_failures=playback_failures or [],
        playback_checked=playback_checked,
        compensation_reports=[("preflight", report), ("postflight", dict(report))],
        focus=FOCUS,
        focus_ids=FOCUS_IDS,
        expected_app_git_sha=expected_app_git_sha,
    )


def test_exact_24_action_certificate_is_a_positive_control() -> None:
    assert issues() == []


def test_final_certificate_rejects_mismatched_build_and_playback_guard_provenance() -> None:
    events = perfect_events()
    first = events[0]
    events[0] = gate.ReviewEventEvidence(
        **{
            **first.__dict__,
            "app_git_sha": "b" * 40,
            "playback_guard_version": "raw-counter-v2",
        }
    )
    found = issues(events=events, expected_app_git_sha="a" * 40)
    assert any("differs from certified executable build" in item for item in found), found
    assert any("content-hash-raw-counter-v3" in item for item in found), found


def test_zero_of_24_is_pending_not_a_capacity_green_certificate() -> None:
    empty = HiddenPilotState(
        policy_sha256=policy_sha256(policy()),
        grants={"Rezan": set(), "Aram": set()},
        session_keys={"Rezan": set(), "Aram": set()},
        completed_keys={"Rezan": set(), "Aram": set()},
        skipped_keys={"Rezan": set(), "Aram": set()},
        unresolved_keys={"Rezan": set(), "Aram": set()},
        corpus_actions={"Rezan": 0, "Aram": 0},
        hidden_actions={"Rezan": 0, "Aram": 0},
    )
    report = perfect_compensation()
    for field in (
        "pilotCorpusActions",
        "pilotHiddenActions",
        "pilotUiActions",
        "pilotHiddenGrants",
        "pilotHiddenResolved",
        "postCutoffEvents",
        "accountingEffectiveEvents",
        "rawPostCutoffEvents",
        "ledgerEntries",
        "rawLedgerEntries",
        "durableOperationReceipts",
        "totalEarnedMicroIqd",
    ):
        report[field] = 0
    found = issues(state=empty, events=[], quality={}, playback_checked=0, compensation=report)
    assert any("0/24 required UI actions" in item for item in found)
    assert any("playback audit covered 0/24" in item for item in found)


def test_19_corpus_actions_cannot_clear_the_gate() -> None:
    events = perfect_events()
    events.pop(9)
    found = issues(events=events, playback_checked=23)
    assert any("23/24 required UI actions" in item for item in found)
    assert any("Rezan has 9/10 corpus" in item for item in found)


def test_any_corpus_or_hidden_skip_is_terminal_red() -> None:
    events = perfect_events()
    events[0] = gate.ReviewEventEvidence(
        events[0].event_id,
        events[0].segment_id,
        events[0].reviewer,
        "skip",
        events[0].source,
        events[0].created_at,
        events[0].timestamp_ms,
    )
    found = issues(events=events, playback_checked=23)
    assert any("certification permits zero skips" in item for item in found)


def test_bad_or_missing_hidden_result_is_red() -> None:
    quality = perfect_quality()
    quality[("Rezan", "hidden-h-1")] = (0, 0.5)
    found = issues(quality=quality)
    assert any("hidden-h-1" in item and "required 1 and 0" in item for item in found)


def _hidden_quality_connection() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.execute(
        """CREATE TABLE spot_checks (
               segment_id TEXT, reviewer TEXT, action,
               submitted_transcript, expected_transcript, noticed, cer
        )"""
    )
    connection.execute(
        """CREATE TABLE speech_segments (
               id TEXT PRIMARY KEY, raw_transcript TEXT, annotated_transcript TEXT,
               verdict_transcript TEXT, human_decision TEXT, verdict TEXT
           )"""
    )
    for reviewer, segment_ids in perfect_state().grants.items():
        for segment_id in segment_ids:
            connection.execute(
                "INSERT INTO speech_segments VALUES (?, 'هەڵە', NULL, 'ڕاست', 'edit', 'human_edit')",
                (segment_id,),
            )
            connection.execute(
                "INSERT INTO spot_checks VALUES (?, ?, 'edit', 'ڕاست', 'ڕاست', 1, 0.0)",
                (segment_id, reviewer),
            )
    return connection


def test_hidden_quality_is_rederived_without_numeric_coercion_or_tolerance() -> None:
    for assignment, expected_error in (
        ("noticed = 1.9", "exact INTEGER 1"),
        ("cer = 0.0000000000001", "exact numeric 0"),
        ("submitted_transcript = 'هەڵە'", "do not derive a correct answer"),
        (
            "submitted_transcript = 'هەڵە', expected_transcript = 'هەڵە'",
            "not the current human answer",
        ),
    ):
        connection = _hidden_quality_connection()
        connection.execute(
            f"UPDATE spot_checks SET {assignment} WHERE segment_id = 'hidden-h-1' AND reviewer = 'Rezan'"
        )
        quality, errors = gate.read_hidden_quality(connection, perfect_state())
        connection.close()
        assert ("Rezan", "hidden-h-1") not in quality
        assert any(expected_error in error for error in errors), errors


def _playback_certificate_connection() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        """
        CREATE TABLE speech_segments (
            id TEXT PRIMARY KEY, review_revision INTEGER, audio_fingerprint TEXT,
            audio_content_hash TEXT, duration_ms INTEGER, alignment_json TEXT
        );
        CREATE TABLE review_compensation_ledger (
            review_event_id INTEGER, policy_version TEXT, decision_revision INTEGER,
            duration_ms INTEGER, reviewer TEXT, segment_id TEXT, source TEXT,
            canonical_work_id TEXT, canonical_identity_kind TEXT
        );
        CREATE TABLE playback_receipts (
            id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT, reviewer TEXT,
            segment_revision INTEGER, audio_fingerprint TEXT, played_ms INTEGER,
            clip_duration_ms INTEGER, coverage_ratio REAL, policy_version INTEGER, started_at_ms INTEGER,
            created_at TEXT, source_start_ms INTEGER, source_end_ms INTEGER
        );
        INSERT INTO speech_segments VALUES (
            'work-h-0', 4, '424242',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1000,
            '{"source_start_ms":0,"source_end_ms":1000}'
        );
        INSERT INTO review_compensation_ledger VALUES
            (864, 'review-iqd-v1-2026-08-21', 4, 1000, 'Rezan', 'work-h-0', 'couch',
             'reviewer-work-v1:5:rezan:audio-segment-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:0:1000',
             'audio_content_hash+source_span');
        """
    )
    return connection


def _insert_certificate_receipt(
    connection: sqlite3.Connection,
    *,
    revision: int = 3,
    reviewer: str = "Rezan",
    fingerprint: str = "a" * 64,
    policy_version: int = gate.PLAYBACK_POLICY_VERSION,
    started_at_ms: int = 1_700_000_000_000,
    source_start_ms: object = 0,
    source_end_ms: object = 1000,
    duration_ms: int = 1000,
) -> None:
    connection.execute(
        """INSERT INTO playback_receipts
               (segment_id, reviewer, segment_revision, audio_fingerprint, played_ms,
                clip_duration_ms, coverage_ratio, policy_version, started_at_ms, created_at,
                source_start_ms, source_end_ms)
             VALUES ('work-h-0', ?, ?, ?, ?, ?, 1.0, ?, ?,
                      '2026-08-22 07:00:00', ?, ?)""",
        (
            reviewer,
            revision,
            fingerprint,
            duration_ms,
            duration_ms,
            policy_version,
            started_at_ms,
            source_start_ms,
            source_end_ms,
        ),
    )


def _manifest_connection() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        """
        CREATE TABLE review_effect_state (
            singleton_key INTEGER PRIMARY KEY,
            effective_after_review_event_id INTEGER,
            effective_after_ledger_id INTEGER,
            created_at TEXT
        );
        CREATE TABLE review_events (
            id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, action TEXT, source TEXT,
            app_git_sha TEXT, playback_guard_version TEXT
        );
        CREATE TABLE speech_segments (
            id TEXT PRIMARY KEY, audio_content_hash TEXT, alignment_json TEXT,
            review_revision INTEGER, duration_ms INTEGER
        );
        CREATE TABLE review_compensation_ledger (
            id INTEGER PRIMARY KEY, entry_id TEXT, entry_key TEXT, policy_version TEXT,
            review_event_id INTEGER, reviewer TEXT, segment_id TEXT, source TEXT,
            decision_revision INTEGER, reverses_entry_id TEXT, delta_micro_iqd INTEGER
        );
        CREATE TABLE playback_receipts (
            id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, segment_revision INTEGER,
            audio_fingerprint TEXT, played_ms INTEGER, clip_duration_ms INTEGER,
            coverage_ratio REAL, policy_version INTEGER, started_at_ms INTEGER,
            source_start_ms INTEGER, source_end_ms INTEGER
        );
        CREATE TABLE human_decision_effect_events (
            id INTEGER PRIMARY KEY, review_event_id INTEGER, segment_id TEXT, reviewer TEXT,
            source TEXT, action TEXT, decision_revision INTEGER
        );
        CREATE TABLE human_decision_effect_reversals (
            effect_event_id INTEGER PRIMARY KEY, operation_id TEXT, created_at TEXT
        );
        CREATE TABLE review_pilot_hidden_keys (
            policy_sha256 TEXT, after_review_event_id INTEGER, reviewer TEXT, segment_id TEXT
        );
        CREATE TABLE spot_checks (
            segment_id TEXT, reviewer TEXT, action TEXT, submitted_transcript TEXT,
            expected_transcript TEXT, noticed INTEGER, cer REAL, created_at TEXT
        );
        CREATE TABLE effective_review_events_v60 (
            review_event_id INTEGER, policy_version TEXT, source TEXT, ledger_id INTEGER
        );
        INSERT INTO review_effect_state VALUES (1, 863, 0, '2026-08-22 07:00:00');
        INSERT INTO review_events VALUES (
            864, 'work-h-0', 'Rezan', 'accept', 'couch',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'content-hash-raw-counter-v3'
        );
        INSERT INTO speech_segments VALUES (
            'work-h-0',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            '{"source_start_ms":0,"source_end_ms":1000}', 4, 1000
        );
        INSERT INTO review_compensation_ledger VALUES (
            1, 'entry-1', 'review-event:864', 'review-iqd-v1-2026-08-21',
            864, 'Rezan', 'work-h-0', 'couch', 4, NULL, 500000
        );
        INSERT INTO review_compensation_ledger VALUES (
            2, 'undo-entry-1', 'undo:22222222-2222-4222-8222-222222222222',
            'review-iqd-v1-2026-08-21', NULL, 'Rezan', 'work-h-0',
            'couch_undo', 4, 'entry-1', -500000
        );
        INSERT INTO playback_receipts VALUES (
            1, 'work-h-0', 'Rezan', 3,
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            1000, 1000, 1.0, 3, 1700000000000, 0, 1000
        );
        INSERT INTO human_decision_effect_events VALUES (
            1, 864, 'work-h-0', 'Rezan', 'couch', 'accept', 4
        );
        INSERT INTO human_decision_effect_reversals VALUES (
            1, '22222222-2222-4222-8222-222222222222', '2026-08-22 07:01:00'
        );
        INSERT INTO effective_review_events_v60 VALUES (
            864, 'review-iqd-v1-2026-08-21', 'couch', 1
        );
        """
    )
    return connection


def test_evidence_manifest_binds_raw_receipt_and_event_bytes() -> None:
    connection = _manifest_connection()
    checked, provenance_errors = gate.raw_event_provenance_issues(connection, 863, "a" * 40)
    assert checked == 1 and provenance_errors == []
    original, manifest = gate.canonical_evidence_manifest(connection, 863)
    assert len(original) == 64
    assert manifest["schemaVersion"] == 60
    tables = manifest["tables"]
    assert len(tables["review_compensation_reversals"]["rows"]) == 1
    assert len(tables["human_decision_effect_reversals"]["rows"]) == 1

    connection.execute("UPDATE playback_receipts SET played_ms=999 WHERE id=1")
    receipt_mutation, _ = gate.canonical_evidence_manifest(connection, 863)
    assert receipt_mutation != original

    connection.execute("UPDATE playback_receipts SET played_ms=1000 WHERE id=1")
    connection.execute(
        "UPDATE playback_receipts SET source_start_ms=1000, source_end_ms=2000 WHERE id=1"
    )
    span_mutation, _ = gate.canonical_evidence_manifest(connection, 863)
    assert span_mutation != original

    connection.execute(
        "UPDATE playback_receipts SET source_start_ms=0, source_end_ms=1000 WHERE id=1"
    )
    connection.execute("UPDATE review_events SET app_git_sha=? WHERE id=864", ("b" * 40,))
    _checked, provenance_errors = gate.raw_event_provenance_issues(connection, 863, "a" * 40)
    assert any("differs from certified executable build" in item for item in provenance_errors)
    event_mutation, _ = gate.canonical_evidence_manifest(connection, 863)
    assert event_mutation != original

    connection.execute("UPDATE review_events SET app_git_sha=? WHERE id=864", ("a" * 40,))
    connection.execute("UPDATE review_compensation_ledger SET delta_micro_iqd=-499999 WHERE id=2")
    reversal_mutation, _ = gate.canonical_evidence_manifest(connection, 863)
    assert reversal_mutation != original
    connection.close()


def test_final_playback_proof_uses_ledger_revision_not_one_second_timestamps() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    # A same-second receipt at the POST-decision revision cannot retroactively authorize the event.
    _insert_certificate_receipt(connection, revision=4)
    reason = gate.final_event_playback_issue(connection, event)
    assert reason is not None and "receipt revision 3" in reason

    _insert_certificate_receipt(connection, started_at_ms=event.timestamp_ms + 1)
    reason = gate.final_event_playback_issue(connection, event)
    assert reason is not None and "after event" in reason

    # The exact pre-decision revision passes, and reviewer identity follows the policy's NOCASE law.
    _insert_certificate_receipt(connection, reviewer="rezan")
    assert gate.final_event_playback_issue(connection, event) is None
    connection.close()


def test_final_playback_proof_requires_one_ledger_row_across_all_policies() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    _insert_certificate_receipt(connection)
    connection.execute(
        """INSERT INTO review_compensation_ledger VALUES
             (864, 'another-policy', 4, 1000, 'Rezan', 'work-h-0', 'couch',
              'reviewer-work-v1:5:rezan:audio-segment-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:0:1000',
              'audio_content_hash+source_span')"""
    )

    reason = gate.final_event_playback_issue(connection, event)
    assert reason is not None and "2 compensation ledger rows" in reason
    connection.close()


def test_final_playback_proof_never_accepts_legacy_policy_one_receipts() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    _insert_certificate_receipt(
        connection,
        policy_version=gate.LEGACY_PLAYBACK_POLICY_VERSION,
        source_start_ms=None,
        source_end_ms=None,
    )

    reason = gate.final_event_playback_issue(connection, event)
    connection.close()
    assert reason is not None and "best canonical coverage 0.00" in reason


def test_final_playback_proof_never_accepts_historical_policy_two_receipts() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    _insert_certificate_receipt(
        connection,
        policy_version=gate.CONTENT_HASH_ONLY_PLAYBACK_POLICY_VERSION,
        source_start_ms=None,
        source_end_ms=None,
    )

    reason = gate.final_event_playback_issue(connection, event)
    connection.close()
    assert reason is not None and "best canonical coverage 0.00" in reason


def test_final_playback_proof_binds_exact_source_span_not_only_duration() -> None:
    event = perfect_events()[0]
    for source_start_ms, source_end_ms, expected in (
        (None, None, "coordinates are not exact integers"),
        (1000, 2000, "disagrees with server-owned (0, 1000)"),
    ):
        connection = _playback_certificate_connection()
        _insert_certificate_receipt(
            connection,
            source_start_ms=source_start_ms,
            source_end_ms=source_end_ms,
        )
        reason = gate.final_event_playback_issue(connection, event)
        connection.close()
        assert reason is not None and expected in reason, reason


def test_final_playback_proof_allows_one_ms_rounding_but_rejects_tenfold_duration() -> None:
    event = perfect_events()[0]

    connection = _playback_certificate_connection()
    connection.execute("UPDATE speech_segments SET duration_ms=1001 WHERE id='work-h-0'")
    connection.execute("UPDATE review_compensation_ledger SET duration_ms=1001 WHERE review_event_id=864")
    _insert_certificate_receipt(connection, duration_ms=1001)
    assert gate.final_event_playback_issue(connection, event) is None
    connection.close()

    connection = _playback_certificate_connection()
    connection.execute("UPDATE speech_segments SET duration_ms=10000 WHERE id='work-h-0'")
    connection.execute("UPDATE review_compensation_ledger SET duration_ms=10000 WHERE review_event_id=864")
    _insert_certificate_receipt(connection, duration_ms=10000)
    reason = gate.final_event_playback_issue(connection, event)
    connection.close()
    assert reason is not None and "differs from exact source span length" in reason


def test_final_playback_proof_requires_the_active_policy_on_the_only_ledger_row() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    connection.execute(
        "UPDATE review_compensation_ledger SET policy_version = 'another-policy' WHERE review_event_id = 864"
    )

    reason = gate.final_event_playback_issue(connection, event)
    assert reason is not None and "required 'review-iqd-v1-2026-08-21'" in reason
    connection.close()


def test_final_certificate_refuses_a_segment_without_server_audio_content_hash() -> None:
    event = perfect_events()[0]
    connection = _playback_certificate_connection()
    connection.execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'work-h-0'")
    _insert_certificate_receipt(connection, fingerprint="id:work-h-0")

    reason = gate.final_event_playback_issue(connection, event)
    connection.close()
    assert reason is not None and "audio content hash is not canonical lowercase 64-hex" in reason


def test_post_decision_revision_drift_cannot_rebind_old_playback_for_corpus_or_hidden() -> None:
    template = perfect_events()[0]
    for event_id, source, receipt_revision in (
        (900, "couch", 3),
        (901, "couch_spot_check", 4),
    ):
        connection = _playback_certificate_connection()
        connection.execute(
            "UPDATE review_compensation_ledger SET review_event_id = ?, source = ?",
            (event_id, source),
        )
        event = gate.ReviewEventEvidence(
            event_id,
            "work-h-0",
            "Rezan",
            "accept",
            source,
            template.created_at,
            template.timestamp_ms,
        )
        _insert_certificate_receipt(connection, revision=receipt_revision)
        assert gate.final_event_playback_issue(connection, event) is None

        # Same fingerprint and duration, but a later row revision can carry a different alignment/span.
        connection.execute("UPDATE speech_segments SET review_revision = 5 WHERE id = 'work-h-0'")
        reason = gate.final_event_playback_issue(connection, event)
        assert reason is not None and "current review revision 5" in reason
        connection.close()


def test_event_outside_the_exact_active_focus_is_red() -> None:
    events = perfect_events()
    first = events[0]
    events[0] = gate.ReviewEventEvidence(
        first.event_id,
        "outside-controlled-focus",
        first.reviewer,
        first.action,
        first.source,
        first.created_at,
        first.timestamp_ms,
    )
    found = issues(events=events)
    assert any("outside the exact active focus" in item for item in found)


def test_all_four_hidden_actions_must_have_playback_evidence_too() -> None:
    found = issues(
        playback_failures=["event 884 (couch_spot_check, Rezan) lacks exact playback evidence"],
        playback_checked=24,
    )
    assert any("couch_spot_check" in item for item in found)


def test_compensation_must_have_one_event_ledger_and_operation_receipt_per_action() -> None:
    report = perfect_compensation()
    report["ledgerEntries"] = 23
    report["durableOperationReceipts"] = 23
    found = issues(compensation=report)
    assert any("ledgerEntries=23" in item for item in found)
    assert any("durableOperationReceipts=23" in item for item in found)


def test_flexible_report_must_be_internally_consistent_and_review_ready() -> None:
    report = flexible_report()
    pool = (
        "123e4567-e89b-42d3-a456-426614174000",
        2,
        "a" * 64,
        "omniasr-7b-test",
        "c" * 64,
    )
    manifest = flexible_manifest(Path("."))
    assert gate.flexible_report_issues(report, pool, manifest) == []
    report["resolutionSummary"]["needsFirstOrSecondReview"] = 1
    report["snapshots"]["offsite"]["fresh"] = False
    found = gate.flexible_report_issues(report, pool, manifest)
    assert any("partition" in item for item in found)
    assert any("offsite snapshot" in item for item in found)


def test_flexible_mode_uses_the_hash_bound_admin_and_not_the_legacy_pilot() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        db_path = root / "cortex-speech.db"
        connection = sqlite3.connect(db_path)
        connection.executescript(
            """
            CREATE TABLE review_pool_registry(
                singleton_key INTEGER, pool_id TEXT, focus_segment_count INTEGER, focus_sha256 TEXT,
                champion_model_version_id TEXT, champion_deployment_sha256 TEXT
            );
            CREATE TABLE review_pool_members(pool_id TEXT, segment_id TEXT);
            CREATE TABLE review_pool_decisions(id INTEGER);
            CREATE TABLE review_pool_reversals(id INTEGER);
            INSERT INTO review_pool_registry VALUES(
                1, '123e4567-e89b-42d3-a456-426614174000', 2,
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'omniasr-7b-test',
                'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'
            );
            INSERT INTO review_pool_members VALUES
                ('123e4567-e89b-42d3-a456-426614174000', 'a'),
                ('123e4567-e89b-42d3-a456-426614174000', 'b');
            """
        )
        connection.commit()
        connection.close()
        manifest = flexible_manifest(root)
        output = io.StringIO()
        with (
            mock.patch.object(gate, "active_pointer", return_value=manifest),
            mock.patch.object(gate, "run_json", return_value=flexible_report()) as run,
            contextlib.redirect_stdout(output),
        ):
            assert gate.main(["--data-dir", raw, "--release-root", raw]) == 0
        payload = json.loads(output.getvalue())
        assert payload["ok"] is True and payload["mode"] == "flexible-pool"
        command = run.call_args.args[0]
        assert command == [
            str(root / "pool_admin.exe"),
            "certify",
            "--db",
            str(db_path),
            "--full-integrity",
            "--require-review-ready",
        ]


def test_flexible_mode_refuses_a_simultaneous_legacy_pilot_policy() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        connection = sqlite3.connect(root / "cortex-speech.db")
        connection.executescript(
            """
            CREATE TABLE review_pool_registry(
                singleton_key INTEGER, pool_id TEXT, focus_segment_count INTEGER, focus_sha256 TEXT,
                champion_model_version_id TEXT, champion_deployment_sha256 TEXT
            );
            CREATE TABLE review_pool_members(pool_id TEXT, segment_id TEXT);
            CREATE TABLE review_pool_decisions(id INTEGER);
            CREATE TABLE review_pool_reversals(id INTEGER);
            INSERT INTO review_pool_registry VALUES(
                1, '123e4567-e89b-42d3-a456-426614174000', 1,
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'omniasr-7b-test',
                'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'
            );
            INSERT INTO review_pool_members VALUES
                ('123e4567-e89b-42d3-a456-426614174000', 'a');
            """
        )
        connection.commit()
        connection.close()
        (root / "review_pilot_policy.json").write_text("{}", encoding="utf-8")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            assert gate.main(["--data-dir", raw, "--release-root", raw]) == 1
        payload = json.loads(output.getvalue())
        assert payload["mode"] == "conflicting"


def test_verify_10_registers_the_mode_selected_gate_without_skip_or_backdating() -> None:
    spec = importlib.util.spec_from_file_location("verify_10_final_pilot_policy", VERIFY_10)
    assert spec and spec.loader
    verify = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = verify
    spec.loader.exec_module(verify)
    matches = [entry for entry in verify.GATES if entry[0] == "review-mode-certification"]
    assert len(matches) == 1
    _name, tier, kind, payload, cwd, probe, _charter = matches[0]
    assert (tier, kind, cwd) == (2, "cmd", verify.APP)
    assert probe is None
    assert str(SCRIPT) in payload and str(verify.EXE) not in payload
    assert "--since" not in payload and "--db" not in payload
    assert "flexible pool" in _charter and "legacy" in _charter


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"FINAL REVIEW PILOT CERTIFICATION POLICY: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
