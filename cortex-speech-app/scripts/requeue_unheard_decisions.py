"""Return decisions made without listening evidence to the review queue.

Enforcement (2026-08-19) refuses a verdict on a clip played below ``MIN_PLAYBACK_COVERAGE``. Five
decisions predate it and were accepted while the guard was only observing: four rejects taken in two
seconds with 0.00 coverage, and one edit at 0.352. A reject permanently removes a clip from a corpus
whose size is the binding constraint, so "leave them, they are only five" is a decision to keep audio
nobody heard out of the dataset forever.

This does NOT delete anything. It clears the decision fields so the clip is pending again and a human
must listen, and it deliberately KEEPS ``annotated_transcript``: on two of these rows the text is
earlier work by other reviewers, and the point is to make someone hear the audio before standing
behind it — not to throw away a correction. ``review_events`` is untouched and append-only, so the
history of who decided what, and when, survives intact.

Every change is written to a JSON record under the data dir before it is applied, naming the row, its
previous state, and the coverage that failed. Dry run by default.

Usage:
    python scripts/requeue_unheard_decisions.py                 # report only
    python scripts/requeue_unheard_decisions.py --apply
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

MIN_PLAYBACK_COVERAGE = 0.85  # mirrors db::MIN_PLAYBACK_COVERAGE


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def decided_without_evidence(conn: sqlite3.Connection, since: str) -> list[dict]:
    """Decisions in the window whose own submit minted no receipt at or above the bar.

    Matched on TIME, like the readiness gate and for the same reason: the decision advances the
    segment's revision, so the receipt it was judged against is always one behind afterwards.
    """
    # ONLY the LATEST decision per segment. Found by the 2026-08-19 hunt, verified: judging every
    # event in the window means a clip that was re-queued once and then properly re-reviewed (new
    # decision, receipt at the bar) still matches on its OLD unevidenced event — so every rerun of
    # --apply wipes the good re-review. The tool is written to be re-runnable; only the decision a
    # segment currently stands on is the tool's business.
    rows = conn.execute(
        """
        SELECT e.segment_id, e.reviewer, e.action, e.created_at
        FROM review_events e
        WHERE e.source = 'couch' AND e.action <> 'skip' AND e.created_at >= ?
          AND e.id = (SELECT MAX(e2.id) FROM review_events e2
                      WHERE e2.segment_id = e.segment_id
                        AND e2.source = 'couch' AND e2.action <> 'skip')
        ORDER BY e.created_at
        """,
        (since,),
    ).fetchall()
    out: list[dict] = []
    for segment_id, reviewer, action, at in rows:
        best = conn.execute(
            """
            SELECT MAX(coverage_ratio) FROM playback_receipts
            WHERE segment_id = ?
              AND reviewer = ?
              AND created_at BETWEEN datetime(?, '-5 seconds') AND datetime(?, '+5 seconds')
            """,
            (segment_id, reviewer, at, at),
        ).fetchone()[0]
        if best is not None and best >= MIN_PLAYBACK_COVERAGE:
            continue
        current = conn.execute(
            """
            SELECT verified, human_decision, verdict, reviewed_by, review_revision,
                   LENGTH(COALESCE(annotated_transcript, ''))
            FROM speech_segments WHERE id = ?
            """,
            (segment_id,),
        ).fetchone()
        if current is None or not current[0]:
            continue  # already pending, or the row is gone — nothing to undo
        out.append(
            {
                "segment_id": segment_id,
                "decided_by": reviewer,
                "action": action,
                "decided_at": at,
                "coverage": best,
                "before": {
                    "verified": current[0],
                    "human_decision": current[1],
                    "verdict": current[2],
                    "reviewed_by": current[3],
                    "review_revision": current[4],
                    "annotated_chars_kept": current[5],
                },
            }
        )
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", default=str(data_dir() / "cortex-speech.db"))
    parser.add_argument(
        "--since",
        default=None,
        help="UTC cutoff; defaults to the first receipt ever minted, and may not precede it",
    )
    parser.add_argument("--apply", action="store_true", help="write the change; otherwise report only")
    args = parser.parse_args()

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
        targets = decided_without_evidence(conn, since)
        print(f"decisions lacking listening evidence since {since} (first receipt: {first_receipt}): {len(targets)}")
        for t in targets:
            cov = "none" if t["coverage"] is None else f"{t['coverage']:.3f}"
            print(
                f"  {t['segment_id'][:8]}  {t['action']:7} by {t['decided_by']:8} at {t['decided_at']}"
                f"  coverage={cov}  keeping {t['before']['annotated_chars_kept']} chars of text"
            )
        if not targets:
            print("nothing to re-queue")
            return 0
        if not args.apply:
            print("\nDRY RUN — pass --apply to re-queue these for a fresh listen")
            return 0

        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        record = data_dir() / f"requeued_unheard_decisions_{stamp}.json"
        record.write_text(
            json.dumps(
                {
                    "reason": "decided while playback enforcement was observe-only; no receipt at or "
                    f"above {MIN_PLAYBACK_COVERAGE}",
                    "written_at": stamp,
                    "annotated_transcript": "preserved — the clip returns to the queue with its text",
                    "review_events": "untouched (append-only history)",
                    "rows": targets,
                },
                indent=2,
                ensure_ascii=False,
            ),
            encoding="utf-8",
            newline="\n",
        )
        print(f"\nsupersession record: {record}")

        for t in targets:
            conn.execute(
                """
                UPDATE speech_segments
                   SET verified = 0, human_decision = NULL, verdict = NULL, reviewed_by = NULL
                 WHERE id = ?
                """,
                (t["segment_id"],),
            )
        conn.commit()
        print(f"re-queued {len(targets)} clip(s) — each keeps its text and must now be heard to be decided")
    finally:
        conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
