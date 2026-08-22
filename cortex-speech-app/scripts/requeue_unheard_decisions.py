"""Return decisions made without listening evidence to the review queue.

Enforcement (2026-08-19) refuses a verdict on a clip played below ``MIN_PLAYBACK_COVERAGE``. Five
decisions predate it and were accepted while the guard was only observing: four rejects taken in two
seconds with 0.00 coverage, and one edit at 0.352. A reject permanently removes a clip from a corpus
whose size is the binding constraint, so "leave them, they are only five" is a decision to keep audio
nobody heard out of the dataset forever.

This is now deliberately REPORT-ONLY. A human decision can also change correction provenance,
correction-memory counters, DPO examples, pay and audit ledgers. Historical rows do not carry a
complete event-bound inverse for all of those effects, so clearing only ``speech_segments`` would
create a false "re-queued" state while derived learning/pay state remained live. ``--apply`` refuses;
an app-owned atomic signed reversal is required.

Usage:
    python scripts/requeue_unheard_decisions.py                 # report only
    python scripts/requeue_unheard_decisions.py --apply
"""

from __future__ import annotations

import argparse
import os
import sqlite3
import sys
from pathlib import Path

from activate_review_pilot import acquire_cortex_lock
from check_playback_enforcement_readiness import uncovered
from check_review_compensation_readiness import POLICY_VERSION

MIN_PLAYBACK_COVERAGE = 0.85  # mirrors db::MIN_PLAYBACK_COVERAGE
VERDICT_FOR_ACTION = {
    "accept": "human_accept",
    "edit": "human_edit",
    "reject": "human_reject",
}


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def _policy_cutoff(conn: sqlite3.Connection) -> tuple[int | None, str | None]:
    rows = conn.execute(
        "SELECT effective_after_event_id FROM review_compensation_policies WHERE policy_version = ?",
        (POLICY_VERSION,),
    ).fetchall()
    if len(rows) != 1 or type(rows[0][0]) is not int or rows[0][0] < 0:
        return None, f"policy {POLICY_VERSION} has no unique non-negative corpus cutoff"
    return rows[0][0], None


def audit_requeue_candidates(conn: sqlite3.Connection, since: str) -> tuple[list[dict], list[dict]]:
    """Return safe legacy targets and rows that must fail closed.

    Re-queue is a destructive supersession. A future app-owned inverse could consider a row only
    when the latest decision log, latest Couch event, and current row are the same immutable identity.
    This offline tool reports candidates but never clears them.
    """
    # ONLY the LATEST decision per segment. Found by the 2026-08-19 hunt, verified: judging every
    # event in the window means a clip that was re-queued once and then properly re-reviewed (new
    # decision, receipt at the bar) still matches on its OLD unevidenced event — so every rerun of
    # --apply wipes the good re-review. The tool is written to be re-runnable; only the decision a
    # segment currently stands on is the tool's business.
    rows = conn.execute(
        """
        SELECT e.id, e.segment_id, e.reviewer, e.action, e.created_at, e.timestamp_ms
        FROM review_events e
        WHERE e.source = 'couch' AND e.action <> 'skip' AND e.created_at >= ?
          AND e.id = (SELECT MAX(e2.id) FROM review_events e2
                      WHERE e2.segment_id = e.segment_id
                        AND e2.source = 'couch' AND e2.action <> 'skip')
        ORDER BY e.created_at
        """,
        (since,),
    ).fetchall()
    targets: list[dict] = []
    blocked: list[dict] = []
    cutoff, cutoff_error = _policy_cutoff(conn)
    for event_id, segment_id, reviewer, action, at, event_timestamp_ms in rows:
        event_failure: str | None = None
        if type(event_id) is not int or event_id <= 0:
            event_failure = f"event id {event_id!r} is not a positive integer"
        elif type(event_timestamp_ms) is not int or event_timestamp_ms <= 0:
            event_failure = f"event timestamp_ms {event_timestamp_ms!r} is not a positive integer"
        elif not isinstance(segment_id, str) or not segment_id.strip():
            event_failure = f"event segment identity {segment_id!r} is invalid"
        elif not isinstance(reviewer, str) or not reviewer.strip():
            event_failure = f"event reviewer identity {reviewer!r} is invalid"
        elif not isinstance(at, str) or not at.strip():
            event_failure = f"event creation time {at!r} is invalid"
        if event_failure is not None:
            blocked.append(
                {
                    "event_id": event_id,
                    "segment_id": segment_id,
                    "decided_by": reviewer,
                    "action": action,
                    "decided_at": at,
                    "event_timestamp_ms": event_timestamp_ms,
                    "coverage": None,
                    "evidence_failure": event_failure,
                    "before": None,
                }
            )
            continue
        current = conn.execute(
            """
            SELECT verified, human_decision, verdict, reviewed_by, review_revision,
                   corrected_at, LENGTH(COALESCE(annotated_transcript, ''))
            FROM speech_segments WHERE id = ?
            """,
            (segment_id,),
        ).fetchone()
        if current is None:
            blocked.append(
                {
                    "event_id": event_id,
                    "segment_id": segment_id,
                    "decided_by": reviewer,
                    "action": action,
                    "decided_at": at,
                    "event_timestamp_ms": event_timestamp_ms,
                    "coverage": None,
                    "evidence_failure": "latest Couch event has no current segment row identity",
                    "before": None,
                }
            )
            continue
        if current[0] == 0:
            continue  # already pending — nothing to undo

        identity_failure: str | None = None
        if type(current[0]) is not int or current[0] != 1:
            identity_failure = f"current verified state {current[0]!r} is not exact INTEGER 1"
        elif action not in VERDICT_FOR_ACTION:
            identity_failure = f"event action {action!r} is not requeueable"
        elif current[1] != action or current[2] != VERDICT_FOR_ACTION[action]:
            identity_failure = "latest Couch event and current decision/action disagree"
        elif not isinstance(current[3], str) or current[3].casefold() != reviewer.casefold():
            identity_failure = "latest Couch event and current reviewer disagree"
        elif current[5] != at:
            identity_failure = "latest Couch event and current corrected_at disagree"
        elif type(current[4]) is not int or current[4] <= 0:
            identity_failure = f"current review_revision {current[4]!r} is invalid"
        else:
            latest_logs = conn.execute(
                """SELECT decision_type, timestamp_ms, human_decision, created_at
                    FROM decision_log WHERE segment_id = ? ORDER BY id DESC LIMIT 1""",
                (segment_id,),
            ).fetchall()
            if len(latest_logs) != 1 or latest_logs[0] != (action, event_timestamp_ms, action, at):
                identity_failure = "latest decision log is not the latest Couch event/current row identity"

        item = {
            "event_id": event_id,
            "segment_id": segment_id,
            "decided_by": reviewer,
            "action": action,
            "decided_at": at,
            "event_timestamp_ms": event_timestamp_ms,
            "coverage": None,
            "evidence_failure": identity_failure,
            "before": {
                "verified": current[0],
                "human_decision": current[1],
                "verdict": current[2],
                "reviewed_by": current[3],
                "review_revision": current[4],
                "corrected_at": current[5],
                "annotated_chars_kept": current[6],
            },
        }
        if identity_failure is not None:
            blocked.append(item)
            continue

        if cutoff_error is not None or cutoff is None:
            item["evidence_failure"] = cutoff_error or "review policy cutoff is unavailable"
            blocked.append(item)
            continue

        all_ledgers = conn.execute(
            """SELECT policy_version, decision_revision, segment_id, reviewer, source
                 FROM review_compensation_ledger WHERE review_event_id = ?""",
            (event_id,),
        ).fetchall()
        is_post_policy = event_id > cutoff
        required_revision: int | None = None
        if is_post_policy:
            if len(all_ledgers) != 1 or all_ledgers[0][0] != POLICY_VERSION:
                item["evidence_failure"] = (
                    f"post-policy event has {len(all_ledgers)} total immutable ledger rows and "
                    f"policy {[row[0] for row in all_ledgers]!r}; required exactly one {POLICY_VERSION}; "
                    "atomic compensation recovery is required"
                )
                blocked.append(item)
                continue
            _policy, decision_revision, ledger_segment, ledger_reviewer, ledger_source = all_ledgers[0]
            if (
                ledger_segment != segment_id
                or ledger_source != "couch"
                or not isinstance(ledger_reviewer, str)
                or ledger_reviewer.casefold() != reviewer.casefold()
            ):
                item["evidence_failure"] = "post-policy event and compensation ledger identity disagree"
                blocked.append(item)
                continue
            if type(decision_revision) is not int or decision_revision <= 0:
                item["evidence_failure"] = f"ledger decision_revision {decision_revision!r} is invalid"
                blocked.append(item)
                continue
            if current[4] != decision_revision:
                item["evidence_failure"] = (
                    f"current revision {current[4]!r} disagrees with paid decision revision {decision_revision}"
                )
                blocked.append(item)
                continue
            required_revision = decision_revision - 1
        else:
            if all_ledgers:
                item["evidence_failure"] = (
                    "legacy event is compensation-ledger-backed; atomic signed reversal is required"
                )
                blocked.append(item)
                continue
            required_revision = current[4] - 1

        evidence_failure = uncovered(
            conn,
            segment_id,
            at,
            reviewer,
            required_revision,
            event_timestamp_ms,
        )
        if evidence_failure is None:
            continue
        item["evidence_failure"] = evidence_failure
        if is_post_policy:
            item["evidence_failure"] += "; paid state requires atomic signed compensation reversal"
            blocked.append(item)
        else:
            targets.append(item)
    return targets, blocked


def decided_without_evidence(conn: sqlite3.Connection, since: str) -> list[dict]:
    """Backward-compatible safe target view used by operators/tests."""
    return audit_requeue_candidates(conn, since)[0]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", default=str(data_dir() / "cortex-speech.db"))
    parser.add_argument(
        "--since",
        default=None,
        help="UTC cutoff; defaults to the first receipt ever minted, and may not precede it",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="request mutation (currently refused: no complete event-bound inverse exists)",
    )
    args = parser.parse_args()

    if args.apply:
        db_path = Path(args.db).expanduser().resolve()
        with acquire_cortex_lock(db_path.parent):
            return run(args)
    return run(args)


def run(args: argparse.Namespace) -> int:

    conn = sqlite3.connect(args.db)
    try:
        # THE VACUITY FENCE. Receipts began on 2026-08-19; before that the mechanism did not exist,
        # so a decision with no receipt is a decision taken before there was anything to record — not
        # evidence that nobody listened. Measured the moment this tool was first run: a window opening
        # 2026-08-01 selected 543 decisions, essentially the entire labelled corpus, and re-queuing
        # them would have thrown away every hour Sewa, Rubar and Lamo have worked. The dry-run default
        # is what caught it; this fence is so the next person does not need to be as lucky.
        first_receipt = conn.execute("SELECT MIN(created_at) FROM playback_receipts").fetchone()[0]
        if first_receipt is None:
            print("no playback receipts exist yet — nothing can be judged unevidenced. Refusing.")
            return 1
        # Normalise BEFORE the lexicographic fence compare: an ISO 'T' separator sorts after every
        # digit, so '2026-08-01T00:00:00' read as later than the first receipt and sailed past the
        # fence into an empty window — the bypass reported "nothing to re-queue" instead of refusing.
        since = (args.since or first_receipt).replace("T", " ")
        if since < first_receipt:
            print(
                "REFUSED: --since " + since + " predates the first receipt ever minted ("
                + first_receipt
                + "). Before that timestamp no decision COULD carry evidence, so every one of "
                "them would be re-queued — which is not a remediation, it is deleting the corpus."
            )
            return 1
        targets, blocked = audit_requeue_candidates(conn, since)
        print(f"decisions lacking listening evidence since {since} (first receipt: {first_receipt}): {len(targets)}")
        for t in targets:
            print(
                f"  {t['segment_id'][:8]}  {t['action']:7} by {t['decided_by']:8} at {t['decided_at']}"
                f"  evidence={t['evidence_failure']}  keeping {t['before']['annotated_chars_kept']} chars of text"
            )
        if blocked:
            print(f"REFUSED/AMBIGUOUS current decisions left untouched: {len(blocked)}")
            for row in blocked:
                print(
                    f"  {str(row['segment_id'])[:8]}  {str(row['action']):7} by {str(row['decided_by']):8} "
                    f"at {row['decided_at']}  reason={row['evidence_failure']}"
                )
            print("No rows were changed: ambiguous or compensated decisions require the app's atomic recovery path.")
            return 1
        if not targets:
            print("nothing to re-queue")
            return 0
        if not args.apply:
            print("\nREPORT ONLY — these legacy rows need an app-owned complete atomic reversal")
            return 0
        print(
            "REFUSED: offline --apply cannot atomically reverse every decision side effect "
            "(learning provenance, correction-memory counters, audit and compensation). No rows changed."
        )
        return 1
    finally:
        conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
