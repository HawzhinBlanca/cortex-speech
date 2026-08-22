"""Gate: is the phone's listening guard deployed and ENFORCING, and is it refusing honest work?

The guard in ``couch::api_decision`` refuses a verdict on a clip that was not played to the bar
(http ``PLAYBACK_EVIDENCE_V3_CONTENT_HASH_RAW_COUNTER_REFUSED``, HTTP 428). The versioned marker
prevents older spectral-fingerprint/stored-ratio builds from impersonating this exact-content-hash,
raw-counter implementation. Enforcement first went live
2026-08-19 on the owner's call, rejects included. This gate ran through that decision and keeps its
job afterwards: prove the DEPLOYED binary is the enforcing one, and surface every decision that
landed without evidence.

It was written while the guard was still only observing, and the informal test for "is it safe yet?"
was: *grep the log; if no reviewer would have been refused, flip it.*

That test is a trap, and it fired on 2026-08-19. The log held zero ``PLAYBACK_EVIDENCE_OBSERVE``
lines — because the running binary was built from the commit BEFORE the guard existed, and because
the last phone decision (2026-08-18 21:19) predated the build that mints receipts at all. Zero
warnings, zero receipts, and the true state of the world was "this code has never executed". Reading
that silence as a pass would have shipped a refusal path into eight reviewers' hands on no evidence.

So absence of warnings is only evidence when the warning could have been emitted. This gate asserts
that first, and refuses to return READY on an empty window:

  1. the binary under test actually CONTAINS the observe marker — otherwise silence is vacuous;
  2. phone decisions were taken in the window, at least ``--min-decisions`` of them;
  3. more than one reviewer is represented, because `timeupdate` fires on a DEVICE, not on a policy:
     twenty clips from one phone say nothing about the other seven reviewers' browsers, and
     enforcement lands on all of them at once;
  4. every decision is covered by a receipt meeting ``MIN_PLAYBACK_COVERAGE`` at the revision,
     decoded-PCM content hash, and exact source span the server resolved — i.e. enforcement would
     have refused nobody.

The reviewers actually represented are NAMED in the output. The gate cannot prove a browser it has
never seen will behave, so it reports its own coverage rather than implying it is complete.

Run:  python scripts/check_playback_enforcement_readiness.py [--exe PATH] [--since ISO] [--min-decisions N]
"""

from __future__ import annotations

import argparse
import datetime as dt
import math
import os
import sqlite3
import sys
from pathlib import Path

from check_review_compensation_readiness import POLICY_VERSION as REVIEW_PAY_POLICY_VERSION
from pilot_focus_contract import canonical_source_span, source_span_duration_issue

# Mirrors db::MIN_PLAYBACK_COVERAGE and db::PLAYBACK_POLICY_VERSION. Pinned by
# test_playback_enforcement_readiness_policy.py so a change on either side cannot drift silently.
MIN_PLAYBACK_COVERAGE = 0.85
LEGACY_PLAYBACK_POLICY_VERSION = 1
CONTENT_HASH_ONLY_PLAYBACK_POLICY_VERSION = 2
PLAYBACK_POLICY_VERSION = 3
HISTORICAL_PLAYBACK_POLICY_VERSIONS = frozenset(
    {LEGACY_PLAYBACK_POLICY_VERSION, CONTENT_HASH_ONLY_PLAYBACK_POLICY_VERSION}
)
ENFORCE_MARKER = b"PLAYBACK_EVIDENCE_V3_CONTENT_HASH_RAW_COUNTER_REFUSED"
DEFAULT_EXE = "src-tauri/target/release/cortex-speech-app.exe"


def default_db_path() -> str:
    """The live library, on whichever platform this runs.

    `os.environ["APPDATA"]` raised KeyError on the macOS and Linux CI runners before the parser had
    even finished building, so the gate died on import-time work no non-Windows caller had asked for
    — including the policy tests, which pass `--db` explicitly and never wanted this value at all.
    Same fallback as build_eval_slices._data_dir.
    """
    override = os.environ.get("CORTEX_DB")
    if override:
        return override
    appdata = os.environ.get("APPDATA")
    root = Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"
    return str(root / "cortex-speech.db")


def binary_can_warn(exe: Path) -> tuple[bool, str]:
    """Is the ENFORCING build deployed? Silence from a build that cannot refuse proves nothing."""
    if not exe.is_file():
        return False, f"{exe} does not exist — nothing to reason about"
    blob = exe.read_bytes()
    if ENFORCE_MARKER not in blob:
        return False, f"{exe.name} does not contain {ENFORCE_MARKER.decode()} — this build does not enforce, so its silence is vacuous"
    return True, f"{exe.name} contains the refusal marker, so this build enforces the listening bar"


def is_canonical_audio_content_hash(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def receipt_source_span_issue(
    receipt_id: object,
    source_start_ms: object,
    source_end_ms: object,
    expected_source_span: tuple[int, int],
) -> str | None:
    """Validate one policy-3 receipt's stored span against the server-owned source window."""
    if type(receipt_id) is not int or receipt_id <= 0:
        return "receipt id is not a positive integer"
    if type(source_start_ms) is not int or type(source_end_ms) is not int:
        return f"policy-3 receipt {receipt_id} source span coordinates are not exact integers"
    if source_start_ms < 0 or source_end_ms <= source_start_ms:
        return (
            f"policy-3 receipt {receipt_id} source span "
            f"({source_start_ms}, {source_end_ms}) is not a non-empty forward range"
        )
    actual = (source_start_ms, source_end_ms)
    if actual != expected_source_span:
        return (
            f"policy-3 receipt {receipt_id} source span {actual} disagrees with "
            f"server-owned {expected_source_span}"
        )
    return None


def decisions_since(conn: sqlite3.Connection, since: str) -> list[tuple[int, str, str, str, object]]:
    """Phone decisions in the window. `skip` writes no verdict and the guard does not gate it."""
    return conn.execute(
        """
        SELECT id, segment_id, reviewer, created_at, timestamp_ms
        FROM review_events
        WHERE source = 'couch' AND action <> 'skip' AND created_at >= ?
        ORDER BY created_at
        """,
        (since,),
    ).fetchall()


def canonical_receipt_coverage(
    row: sqlite3.Row | tuple[object, ...],
    *,
    expected_duration_ms: int | None = None,
    allowed_policy_versions: frozenset[int] = frozenset({PLAYBACK_POLICY_VERSION}),
) -> tuple[float | None, str | None]:
    """Recompute one receipt from its integer authority instead of trusting its stored REAL.

    Row order is ``id, played_ms, clip_duration_ms, coverage_ratio, policy_version``.  This mirrors
    the restore validator and closes a durable false-pass where ``played_ms=0`` was paired with a
    manually inconsistent ``coverage_ratio=1``.
    """
    receipt_id, played_ms, clip_duration_ms, stored_coverage, policy_version = row
    if type(receipt_id) is not int or receipt_id <= 0:
        return None, "receipt id is not a positive integer"
    if type(played_ms) is not int or played_ms < 0:
        return None, f"receipt {receipt_id} played_ms is not a non-negative integer"
    if type(clip_duration_ms) is not int or clip_duration_ms <= 0:
        return None, f"receipt {receipt_id} clip_duration_ms is not a positive integer"
    if type(policy_version) is not int or policy_version not in allowed_policy_versions:
        return None, (
            f"receipt {receipt_id} policy_version={policy_version!r}; "
            f"required one of {sorted(allowed_policy_versions)}"
        )
    if type(stored_coverage) not in (int, float) or not math.isfinite(float(stored_coverage)):
        return None, f"receipt {receipt_id} coverage_ratio is not finite numeric evidence"
    if expected_duration_ms is not None and clip_duration_ms != expected_duration_ms:
        return None, (
            f"receipt {receipt_id} duration {clip_duration_ms}ms disagrees with "
            f"server-owned {expected_duration_ms}ms"
        )
    computed = min(1.0, played_ms / clip_duration_ms)
    stored = float(stored_coverage)
    tolerance = max(1e-12, abs(computed) * sys.float_info.epsilon * 8.0)
    if abs(stored - computed) > tolerance:
        return None, (
            f"receipt {receipt_id} stored coverage {stored:.6f} disagrees with "
            f"raw media counters {computed:.6f}"
        )
    return computed, None


def playback_receipt_semantic_issues(conn: sqlite3.Connection) -> tuple[int, list[str]]:
    """Audit every live receipt against the canonical writer/restore invariants."""
    rows = conn.execute(
        """
        SELECT p.id, p.segment_id, p.segment_revision, p.audio_fingerprint,
               p.played_ms, p.clip_duration_ms, p.coverage_ratio, p.policy_version,
               p.started_at_ms, p.source_start_ms, p.source_end_ms,
               s.id, COALESCE(s.review_revision, 0),
               CAST(NULLIF(TRIM(COALESCE(s.audio_content_hash, '')), '') AS TEXT),
               COALESCE(s.duration_ms, 0), s.alignment_json
          FROM playback_receipts p
          LEFT JOIN speech_segments s ON s.id = p.segment_id
         ORDER BY p.id
        """
    ).fetchall()
    errors: list[str] = []
    for row in rows:
        (
            receipt_id,
            segment_id,
            segment_revision,
            stored_audio_identity,
            played_ms,
            clip_duration_ms,
            stored_coverage,
            policy_version,
            started_at_ms,
            receipt_source_start_ms,
            receipt_source_end_ms,
            current_segment_id,
            current_revision,
            current_content_hash,
            current_duration,
            current_alignment_json,
        ) = row
        _computed, reason = canonical_receipt_coverage(
            (receipt_id, played_ms, clip_duration_ms, stored_coverage, policy_version),
            allowed_policy_versions=HISTORICAL_PLAYBACK_POLICY_VERSIONS
            | frozenset({PLAYBACK_POLICY_VERSION}),
        )
        if reason:
            errors.append(reason)
            continue
        if type(started_at_ms) is not int or started_at_ms < 0:
            errors.append(f"receipt {receipt_id} started_at_ms is not a non-negative integer")
            continue
        if not isinstance(segment_id, str) or not segment_id.strip():
            errors.append(f"receipt {receipt_id} has an empty segment id")
            continue
        if type(segment_revision) is not int or segment_revision < 0:
            errors.append(f"receipt {receipt_id} has an invalid segment revision")
            continue
        if not isinstance(stored_audio_identity, str) or not stored_audio_identity.strip():
            errors.append(f"receipt {receipt_id} has an empty stored audio identity")
            continue
        if current_segment_id is None:
            errors.append(f"receipt {receipt_id} points to missing segment {segment_id!r}")
            continue
        if segment_revision > current_revision:
            errors.append(f"receipt {receipt_id} is from a future segment revision")
            continue
        # Policy 1 stored the v50 64-bit spectral candidate in the legacy column. Preserve it as
        # historical/readable, but never reinterpret it as decoded-PCM identity and never authorize
        # a current-policy decision from it.
        if policy_version == LEGACY_PLAYBACK_POLICY_VERSION:
            continue
        if not is_canonical_audio_content_hash(stored_audio_identity):
            errors.append(f"policy-{policy_version} receipt {receipt_id} has a non-canonical audio content hash")
            continue
        if not is_canonical_audio_content_hash(current_content_hash):
            errors.append(
                f"policy-{policy_version} receipt {receipt_id} cannot be validated because segment {segment_id!r} "
                "has no canonical server-derived audio content hash"
            )
            continue
        if policy_version == PLAYBACK_POLICY_VERSION:
            expected_source_span, span_reason = canonical_source_span(current_alignment_json)
            if expected_source_span is None:
                errors.append(f"policy-3 receipt {receipt_id} cannot be validated: {span_reason}")
                continue
            duration_issue = source_span_duration_issue(
                current_duration,
                expected_source_span,
                subject=f"segment {segment_id!r} duration",
            )
            if duration_issue:
                errors.append(f"policy-3 receipt {receipt_id} cannot be validated: {duration_issue}")
                continue
            span_issue = receipt_source_span_issue(
                receipt_id,
                receipt_source_start_ms,
                receipt_source_end_ms,
                expected_source_span,
            )
            if span_issue:
                errors.append(span_issue)
                continue
        # Every content-hash receipt at the current revision was minted from this row. Old revisions can
        # legitimately name replaced bytes, but current rows must match identity and denominator
        # whether or not the raw counters clear the listening bar.
        if segment_revision == current_revision:
            if (
                stored_audio_identity != current_content_hash
                or type(current_duration) is not int
                or current_duration <= 0
                or clip_duration_ms != current_duration
            ):
                errors.append(
                    f"policy-{policy_version} receipt {receipt_id} disagrees with "
                    "its server-owned audio identity"
                )
    return len(rows), errors


def corpus_receipt_revision_for_event(
    conn: sqlite3.Connection,
    event_id: int,
    segment_id: str,
    reviewer: str,
    source: str = "couch",
) -> tuple[int | None, str | None]:
    """Return the one immutable receipt revision for a paid corpus event.

    A corpus decision increments ``speech_segments.review_revision`` once.  Migration 57 snapshots
    that post-decision revision into the immutable compensation ledger, so the receipt that
    authorized it must be exactly one revision earlier.  Timestamp-nearest inference is not proof:
    an older listen at the same audio content hash can belong to different text.
    """
    if type(event_id) is not int or event_id <= 0:
        return None, f"event id {event_id!r} is not a positive integer"
    if not isinstance(segment_id, str) or not segment_id.strip():
        return None, f"event {event_id} segment identity {segment_id!r} is invalid"
    if not isinstance(reviewer, str) or not reviewer.strip():
        return None, f"event {event_id} reviewer identity {reviewer!r} is invalid"
    rows = conn.execute(
        """SELECT decision_revision, segment_id, reviewer, source, policy_version
             FROM review_compensation_ledger
            WHERE review_event_id = ?""",
        (event_id,),
    ).fetchall()
    if len(rows) != 1:
        return None, f"event {event_id} has {len(rows)} immutable compensation rows; required exactly one"
    decision_revision, ledger_segment, ledger_reviewer, ledger_source, ledger_policy = rows[0]
    if (
        ledger_segment != segment_id
        or ledger_source != source
        or not isinstance(ledger_reviewer, str)
        or ledger_reviewer.casefold() != reviewer.casefold()
    ):
        return None, f"event {event_id} and immutable compensation ledger identity disagree"
    if ledger_policy != REVIEW_PAY_POLICY_VERSION:
        return None, f"event {event_id} ledger policy {ledger_policy!r} is not the active review policy"
    if source != "couch":
        return None, f"event {event_id} source {source!r} is not corpus Couch review"
    if type(decision_revision) is not int or decision_revision <= 0:
        return None, f"event {event_id} decision revision {decision_revision!r} cannot name a corpus receipt"
    return decision_revision - 1, None


def uncovered(
    conn: sqlite3.Connection,
    segment_id: str,
    decided_at: str,
    reviewer: str | None,
    required_revision: int,
    decision_timestamp_ms: object,
) -> str | None:
    """Was THIS decision backed by a receipt at the moment it was made?

    ``required_revision`` is mandatory and comes from immutable decision evidence (or a destructive
    legacy repair's strict current-revision relationship).  Time is only an upper bound.  Inferring
    the revision from the newest receipt before the event false-passed an old listen after the row's
    text/revision changed: the content hash proves decoded PCM, not which draft was judged.
    """
    if type(required_revision) is not int or required_revision < 0:
        return f"required receipt revision {required_revision!r} is not a non-negative integer"
    if type(decision_timestamp_ms) is not int or decision_timestamp_ms <= 0:
        return f"decision timestamp_ms {decision_timestamp_ms!r} is not a positive integer"
    identity_row = conn.execute(
        """
        SELECT CAST(NULLIF(TRIM(COALESCE(audio_content_hash, '')), '') AS TEXT),
               COALESCE(duration_ms, 0), alignment_json
        FROM speech_segments WHERE id = ?
        """,
        (segment_id,),
    ).fetchone()
    if identity_row is None:
        return "segment is missing, so its decision-time audio identity cannot be proved"
    content_hash = identity_row[0]
    if not is_canonical_audio_content_hash(content_hash):
        return "segment has no canonical server-derived audio content hash, so exact playback identity cannot be proved"
    duration_ms = identity_row[1]
    if type(duration_ms) is not int or duration_ms <= 0:
        return f"server-owned clip duration {duration_ms!r}ms is not valid"
    expected_source_span, span_reason = canonical_source_span(identity_row[2])
    if expected_source_span is None:
        return f"exact playback source span cannot be proved: {span_reason}"
    duration_issue = source_span_duration_issue(
        duration_ms,
        expected_source_span,
        subject="server-owned clip duration",
    )
    if duration_issue:
        return f"exact playback source span cannot be proved: {duration_issue}"

    # Bind the retrospective proof to the same server-owned audio identity as
    # db::has_sufficient_playback_evidence.  Revision alone is insufficient: a stale receipt for
    # different bytes can share a segment/revision and must never make the current audio look heard.
    reviewer_clause = "reviewer IS NULL" if reviewer is None else "reviewer = ? COLLATE NOCASE"
    parameters: tuple[object, ...] = (
        (segment_id, required_revision, content_hash, decided_at)
        if reviewer is None
        else (segment_id, reviewer, required_revision, content_hash, decided_at)
    )
    candidates = conn.execute(
        f"""
        SELECT id, played_ms, clip_duration_ms, coverage_ratio, policy_version, started_at_ms,
               source_start_ms, source_end_ms
        FROM playback_receipts
        WHERE segment_id = ? AND {reviewer_clause}
          AND typeof(segment_revision) = 'integer' AND segment_revision = ?
          AND audio_fingerprint = ?
          AND typeof(policy_version) = 'integer' AND policy_version = {PLAYBACK_POLICY_VERSION}
          AND created_at <= datetime(?)
        """,
        parameters,
    ).fetchall()
    valid_coverages: list[float] = []
    invalid_reasons: list[str] = []
    for candidate in candidates:
        receipt = candidate[:5]
        started_at_ms = candidate[5]
        source_start_ms = candidate[6]
        source_end_ms = candidate[7]
        if type(started_at_ms) is not int or started_at_ms < 0:
            invalid_reasons.append(f"receipt {receipt[0]} started_at_ms is not a non-negative integer")
            continue
        if started_at_ms > decision_timestamp_ms:
            invalid_reasons.append(
                f"receipt {receipt[0]} started at {started_at_ms} after decision at {decision_timestamp_ms}"
            )
            continue
        span_issue = receipt_source_span_issue(
            receipt[0], source_start_ms, source_end_ms, expected_source_span
        )
        if span_issue:
            invalid_reasons.append(span_issue)
            continue
        coverage, reason = canonical_receipt_coverage(receipt, expected_duration_ms=duration_ms)
        if reason:
            invalid_reasons.append(reason)
        elif coverage is not None:
            valid_coverages.append(coverage)
    best = max(valid_coverages, default=0.0)
    if best < MIN_PLAYBACK_COVERAGE:
        invalid = f"; invalid evidence: {invalid_reasons[0]}" if invalid_reasons else ""
        return (
            f"revision {required_revision} best canonical coverage {best:.2f} "
            f"< {MIN_PLAYBACK_COVERAGE:.2f}{invalid}"
        )
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", default=default_db_path())
    parser.add_argument("--exe", default=DEFAULT_EXE, type=Path)
    parser.add_argument(
        "--since",
        default=None,
        help="ISO timestamp; defaults to the binary's build time, which is when it could first warn",
    )
    parser.add_argument("--min-decisions", type=int, default=20)
    parser.add_argument("--min-reviewers", type=int, default=2)
    args = parser.parse_args()

    print("PLAYBACK ENFORCEMENT READINESS")
    failures = 0

    can_warn, why = binary_can_warn(args.exe)
    if can_warn:
        print(f"PASS [binary]: {why}")
    else:
        failures += 1
        print(f"FAIL [binary]: {why}")

    since = args.since
    if since is None:
        if args.exe.is_file():
            # UTC, because that is what the rows are in. SQLite's datetime('now') is UTC and the
            # tracing log stamps Zulu; deriving the cutoff from a LOCAL-time mtime silently discarded
            # every decision in the last UTC-offset hours. Measured 2026-08-19 on a UTC+3 box while a
            # reviewer was actively working: the gate reported "0 decisions" against 9 real ones and
            # concealed the first genuine would-be refusal.
            since = dt.datetime.fromtimestamp(args.exe.stat().st_mtime, dt.timezone.utc).strftime(
                "%Y-%m-%d %H:%M:%S"
            )
        else:
            since = "9999-01-01 00:00:00"
    print(f"       window : decisions at or after {since}")

    conn = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    try:
        receipt_count, semantic_errors = playback_receipt_semantic_issues(conn)
        print(f"       playback receipts audited: {receipt_count}")
        if semantic_errors:
            failures += 1
            print(
                f"FAIL [receipt integrity]: {len(semantic_errors)} receipt(s) violate the "
                "canonical raw-counter/policy/audio-identity/source-span contract"
            )
            for reason in semantic_errors[:10]:
                print(f"  {reason}")
            if len(semantic_errors) > 10:
                print(f"  ... and {len(semantic_errors) - 10} more")
        else:
            print(
                "PASS [receipt integrity]: every receipt recomputes from canonical raw counters "
                "and policy-3 spans match server identity"
            )

        rows = decisions_since(conn, since)
        print(f"       phone decisions in window: {len(rows)}")
        if len(rows) < args.min_decisions:
            failures += 1
            print(
                f"FAIL [evidence]: {len(rows)} decision(s) since the guard went live, need "
                f"{args.min_decisions} — an empty window cannot show that enforcement refuses nobody"
            )
        else:
            print(f"PASS [evidence]: {len(rows)} decision(s) is a real sample")

        represented = sorted({who for _, _, who, _, _ in rows})
        if rows:
            print(f"       reviewers represented: {len(represented)} ({', '.join(represented)})")
        if rows and len(represented) < args.min_reviewers:
            failures += 1
            print(
                f"FAIL [devices]: only {len(represented)} reviewer(s) exercised the guard, need "
                f"{args.min_reviewers} — playback ticks come from a browser, not from a policy"
            )
        elif rows:
            print(f"PASS [devices]: {len(represented)} distinct reviewer(s) exercised the guard")

        refused = []
        for event_id, seg, who, at, timestamp_ms in rows:
            required_revision, revision_error = corpus_receipt_revision_for_event(
                conn, event_id, seg, who, "couch"
            )
            why = revision_error
            if why is None and required_revision is not None:
                why = uncovered(conn, seg, at, who, required_revision, timestamp_ms)
            if why is not None:
                refused.append((seg, who, at, why))
        if refused:
            failures += 1
            print(f"FAIL [coverage]: enforcement REFUSED {len(refused)} of {len(rows)} decision(s)")
            for seg, who, at, why in refused[:10]:
                print(f"  {seg} by {who} at {at}: {why}")
            if len(refused) > 10:
                print(f"  ... and {len(refused) - 10} more")
        elif rows:
            print(f"PASS [coverage]: all {len(rows)} decision(s) carry a receipt at or above the bar")
    except sqlite3.Error as error:
        failures += 1
        print(f"FAIL [database]: playback evidence cannot be audited read-only: {error}")
    finally:
        conn.close()

    if failures:
        print(f"PLAYBACK ENFORCEMENT READINESS: NOT READY — {failures} check(s) failed")
        return 1
    print("PLAYBACK ENFORCEMENT READINESS: READY — the guard may be switched to refusing")
    return 0


if __name__ == "__main__":
    sys.exit(main())
