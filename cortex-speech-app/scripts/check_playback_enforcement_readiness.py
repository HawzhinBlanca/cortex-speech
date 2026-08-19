"""Gate: may the phone's listening guard be switched from OBSERVING to REFUSING?

The guard in ``couch::api_decision`` currently asks the real question and only logs the answer
(``PLAYBACK_EVIDENCE_OBSERVE``). Turning it into a refusal is a one-line change, and the informal
test for "is it safe yet?" was: *grep the log; if no reviewer would have been refused, flip it.*

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
  4. every decision is covered by a receipt meeting ``MIN_PLAYBACK_COVERAGE`` at the revision and
     fingerprint the server resolved — i.e. enforcement would have refused nobody.

The reviewers actually represented are NAMED in the output. The gate cannot prove a browser it has
never seen will behave, so it reports its own coverage rather than implying it is complete.

Run:  python scripts/check_playback_enforcement_readiness.py [--exe PATH] [--since ISO] [--min-decisions N]
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import sqlite3
import sys
from pathlib import Path

# Mirrors db::MIN_PLAYBACK_COVERAGE and db::PLAYBACK_POLICY_VERSION. Pinned by
# test_playback_enforcement_readiness_policy.py so a change on either side cannot drift silently.
MIN_PLAYBACK_COVERAGE = 0.90
PLAYBACK_POLICY_VERSION = 1
OBSERVE_MARKER = b"PLAYBACK_EVIDENCE_OBSERVE"
DEFAULT_EXE = "src-tauri/target/release/cortex-speech-app.exe"


def default_db_path() -> str:
    return os.environ.get("CORTEX_DB") or os.path.join(
        os.environ["APPDATA"], "cortex-speech", "cortex-speech.db"
    )


def binary_can_warn(exe: Path) -> tuple[bool, str]:
    """Is the observe marker compiled into this binary? Silence from a build without it proves nothing."""
    if not exe.is_file():
        return False, f"{exe} does not exist — nothing to reason about"
    blob = exe.read_bytes()
    if OBSERVE_MARKER not in blob:
        return False, f"{exe.name} does not contain {OBSERVE_MARKER.decode()} — it cannot warn, so its silence is vacuous"
    return True, f"{exe.name} contains the observe marker, so a refusal WOULD have been logged"


def decisions_since(conn: sqlite3.Connection, since: str) -> list[tuple[str, str, str]]:
    """Phone decisions in the window. `skip` writes no verdict and the guard does not gate it."""
    return conn.execute(
        """
        SELECT segment_id, reviewer, created_at
        FROM review_events
        WHERE source = 'couch' AND action <> 'skip' AND created_at >= ?
        ORDER BY created_at
        """,
        (since,),
    ).fetchall()


def uncovered(conn: sqlite3.Connection, segment_id: str) -> str | None:
    """Reproduce db::has_sufficient_playback_evidence against the row's CURRENT identity.

    Same precedence as the server: the revision and fingerprint are read from the segment, never
    from anything the client sent. Returns a human-readable reason when the guard would refuse.
    """
    row = conn.execute(
        """
        SELECT COALESCE(review_revision, 0),
               NULLIF(TRIM(COALESCE(audio_fingerprint, '')), '')
        FROM speech_segments WHERE id = ?
        """,
        (segment_id,),
    ).fetchone()
    if row is None:
        return "segment no longer exists"
    revision, fingerprint = row
    if not fingerprint:
        fingerprint = f"id:{segment_id}"
    best = conn.execute(
        """
        SELECT MAX(coverage_ratio) FROM playback_receipts
        WHERE segment_id = ? AND segment_revision = ? AND audio_fingerprint = ?
        """,
        (segment_id, revision, fingerprint),
    ).fetchone()[0]
    if best is None:
        return f"no receipt at revision {revision}"
    if best < MIN_PLAYBACK_COVERAGE:
        return f"best coverage {best:.2f} < {MIN_PLAYBACK_COVERAGE:.2f} at revision {revision}"
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
            since = dt.datetime.fromtimestamp(args.exe.stat().st_mtime).strftime("%Y-%m-%d %H:%M:%S")
        else:
            since = "9999-01-01 00:00:00"
    print(f"       window : decisions at or after {since}")

    conn = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    try:
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

        represented = sorted({who for _, who, _ in rows})
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

        refused = [(seg, who, at, why) for seg, who, at in rows if (why := uncovered(conn, seg))]
        if refused:
            failures += 1
            print(f"FAIL [coverage]: enforcement would have REFUSED {len(refused)} of {len(rows)} decision(s)")
            for seg, who, at, why in refused[:10]:
                print(f"  {seg} by {who} at {at}: {why}")
            if len(refused) > 10:
                print(f"  ... and {len(refused) - 10} more")
        elif rows:
            print(f"PASS [coverage]: all {len(rows)} decision(s) carry a receipt at or above the bar")
    finally:
        conn.close()

    if failures:
        print(f"PLAYBACK ENFORCEMENT READINESS: NOT READY — {failures} check(s) failed")
        return 1
    print("PLAYBACK ENFORCEMENT READINESS: READY — the guard may be switched to refusing")
    return 0


if __name__ == "__main__":
    sys.exit(main())
