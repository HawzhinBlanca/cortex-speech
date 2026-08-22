"""Finish decisions the two-write desktop path left half-written, and stamp owner rights it skipped.

Two live-data repairs, both found 2026-08-20:

  1. NINE rows carried `human_decision` with `verified = 0` — real desktop review the export cannot
     see, because `finalize` was derived from a CAS token only the phone supplies. Each is finalized
     ONLY when one legacy decision-log/event row proves the current reviewer, action and time, and
     playback proves exactly ``receipt_revision + 1 == current_review_revision``. Ambiguous or
     post-policy paid state is left alone and reported; this tool cannot rewrite compensation.

  2. Clips from two owner recordings carry no `rights_license`. Canon: every clip the owner supplies
     is `owner-full-rights`, unrestricted, rights clearance CLOSED. These are the owner's own podcast
     episodes sitting beside 23 stamped ones; the gap is a missing stamp, not an open question.

Dry run by default.
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

OWNER_LICENSE = "owner-full-rights"
OWNER_USE = "unrestricted: train, evaluate, publish, redistribute, commercial"
VERDICT_FOR_ACTION = {
    "accept": "human_accept",
    "edit": "human_edit",
    "reject": "human_reject",
}


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", default=str(data_dir() / "cortex-speech.db"))
    ap.add_argument("--apply", action="store_true")
    ap.add_argument(
        "--stamp-like",
        action="append",
        default=[],
        help="SQL LIKE over audio_path naming OWNER-supplied recordings to stamp; repeatable, required",
    )
    args = ap.parse_args()

    if args.apply:
        db_path = Path(args.db).expanduser().resolve()
        with acquire_cortex_lock(db_path.parent):
            return run(args)
    return run(args)


def _corpus_cutoff(con: sqlite3.Connection) -> tuple[int | None, str | None]:
    rows = con.execute(
        "SELECT effective_after_event_id FROM review_compensation_policies WHERE policy_version = ?",
        (POLICY_VERSION,),
    ).fetchall()
    if len(rows) != 1 or type(rows[0][0]) is not int or rows[0][0] < 0:
        return None, f"policy {POLICY_VERSION} has no unique non-negative corpus cutoff"
    return rows[0][0], None


def legacy_finalizable_reviews(con: sqlite3.Connection) -> tuple[list[dict], list[dict]]:
    """Select only provable legacy half-writes; never infer identity from a nearby receipt."""
    rows = con.execute(
        """SELECT id, human_decision, verdict, corrected_at, reviewed_by, review_revision
             FROM speech_segments
            WHERE COALESCE(human_decision,'') <> '' AND verified = 0"""
    ).fetchall()
    finalizable: list[dict] = []
    ambiguous: list[dict] = []
    cutoff, cutoff_error = _corpus_cutoff(con)
    for sid, action, verdict, corrected_at, reviewer, revision in rows:
        failure: str | None = None
        decision_timestamp_ms: object = None
        if not isinstance(sid, str) or not sid.strip():
            failure = f"current segment identity {sid!r} is invalid"
        elif action not in VERDICT_FOR_ACTION:
            failure = f"unsupported current action {action!r}"
        elif verdict != VERDICT_FOR_ACTION[action]:
            failure = f"current verdict {verdict!r} disagrees with action {action!r}"
        elif not isinstance(corrected_at, str) or not corrected_at.strip():
            failure = "current decision has no exact corrected_at identity"
        elif type(revision) is not int or revision <= 0:
            failure = f"current review_revision {revision!r} cannot follow a receipt revision"
        elif reviewer is None:
            logs = con.execute(
                """SELECT id, timestamp_ms FROM decision_log
                    WHERE segment_id = ? AND decision_type = ? AND human_decision = ?
                      AND created_at = ?""",
                (sid, action, action, corrected_at),
            ).fetchall()
            if len(logs) != 1:
                failure = f"desktop current decision has {len(logs)} exact decision-log identities"
            else:
                latest = con.execute(
                    """SELECT id, decision_type, timestamp_ms, human_decision, created_at
                         FROM decision_log WHERE segment_id = ? ORDER BY id DESC LIMIT 1""",
                    (sid,),
                ).fetchall()
                log_id, decision_timestamp_ms = logs[0]
                if latest != [(log_id, action, decision_timestamp_ms, action, corrected_at)]:
                    failure = "desktop current decision is not the latest decision-log identity"
        elif not isinstance(reviewer, str) or not reviewer.strip():
            failure = f"current reviewer identity {reviewer!r} is invalid"
        else:
            events = con.execute(
                """SELECT id, timestamp_ms FROM review_events
                    WHERE segment_id = ? AND reviewer = ? COLLATE NOCASE
                      AND action = ? AND source = 'couch' AND created_at = ?""",
                (sid, reviewer, action, corrected_at),
            ).fetchall()
            if len(events) != 1:
                failure = f"attributed current decision has {len(events)} exact Couch event identities"
            else:
                event_id, decision_timestamp_ms = events[0]
                latest_event = con.execute(
                    """SELECT id FROM review_events
                         WHERE segment_id = ? AND source = 'couch' AND action <> 'skip'
                         ORDER BY id DESC LIMIT 1""",
                    (sid,),
                ).fetchall()
                latest_log = con.execute(
                    """SELECT decision_type, timestamp_ms, human_decision, created_at
                         FROM decision_log WHERE segment_id = ? ORDER BY id DESC LIMIT 1""",
                    (sid,),
                ).fetchall()
                if latest_event != [(event_id,)] or latest_log != [
                    (action, decision_timestamp_ms, action, corrected_at)
                ]:
                    failure = "attributed current decision is not the latest event/log identity"
                else:
                    ledger_count = con.execute(
                        "SELECT COUNT(*) FROM review_compensation_ledger WHERE review_event_id = ?",
                        (event_id,),
                    ).fetchone()[0]
                    if ledger_count:
                        failure = "current decision is compensation-ledger-backed; legacy repair is forbidden"
                    elif cutoff_error is not None or cutoff is None:
                        failure = cutoff_error
                    elif type(event_id) is not int or event_id > cutoff:
                        failure = "current decision is post-policy and lacks an atomic compensation repair path"

        if failure is None:
            failure = uncovered(
                con,
                sid,
                corrected_at,
                reviewer,
                revision - 1,
                decision_timestamp_ms,
            )
        item = {
            "segment_id": sid,
            "action": action,
            "verdict": verdict,
            "corrected_at": corrected_at,
            "reviewer": reviewer,
            "review_revision": revision,
            "failure": failure,
        }
        (finalizable if failure is None else ambiguous).append(item)
    return finalizable, ambiguous


def run(args: argparse.Namespace) -> int:
    con = sqlite3.connect(args.db)
    try:
        if args.apply:
            con.execute("BEGIN IMMEDIATE")
        print("=== 1. decided-but-unverified rows ===")
        finalizable, ambiguous = legacy_finalizable_reviews(con)
        for row in finalizable:
            print(f"  finalize {row['segment_id'][:8]} {row['action']:7} exact legacy evidence verified")
        for row in ambiguous:
            print(f"  LEAVE    {row['segment_id'][:8]} {str(row['action']):7} — {row['failure']}")

        print("\n=== 2. clips with no rights stamp ===")
        overall = con.execute(
            "SELECT COUNT(*), COUNT(DISTINCT audio_path) FROM speech_segments "
            " WHERE COALESCE(TRIM(rights_license),'') = ''"
        ).fetchone()
        print(f"  unstamped overall: {overall[0]} clip(s) across {overall[1]} file(s)")
        if not args.stamp_like:
            print("  no --stamp-like given: stamping nothing. Canon covers OWNER-supplied audio only,")
            print("  and a blanket stamp would claim third-party corpora as the owner's.")
            total_unstamped = 0
        else:
            where = " OR ".join(["audio_path LIKE ?"] * len(args.stamp_like))
            targets = con.execute(
                f"SELECT audio_path, COUNT(*) FROM speech_segments "
                f" WHERE COALESCE(TRIM(rights_license),'') = '' AND ({where}) GROUP BY audio_path ORDER BY 2 DESC",
                args.stamp_like,
            ).fetchall()
            for path, n in targets:
                print(f"  stamp {n:5} clips  {path}")
            total_unstamped = sum(n for _, n in targets)

        if not args.apply:
            print(f"\nDRY RUN — would finalize {len(finalizable)}, stamp {total_unstamped}. Pass --apply.")
            return 0

        for row in finalizable:
            changed = con.execute(
                """UPDATE speech_segments SET verified = 1, updated_at = datetime('now')
                    WHERE id = ? AND verified = 0 AND human_decision IS ? AND verdict IS ?
                      AND corrected_at IS ? AND reviewed_by IS ? AND review_revision = ?""",
                (
                    row["segment_id"],
                    row["action"],
                    row["verdict"],
                    row["corrected_at"],
                    row["reviewer"],
                    row["review_revision"],
                ),
            ).rowcount
            if changed != 1:
                con.rollback()
                print(f"REFUSED: {row['segment_id']} changed after proof; no repairs were committed")
                return 1
        if args.stamp_like:
            where = " OR ".join(["audio_path LIKE ?"] * len(args.stamp_like))
            con.execute(
                f"UPDATE speech_segments SET rights_license = ?, rights_permitted_use = ?, "
                f"       rights_consent_basis = 'owner declaration 2026-08-14', updated_at = datetime('now') "
                f" WHERE COALESCE(TRIM(rights_license),'') = '' AND ({where})",
                (OWNER_LICENSE, OWNER_USE, *args.stamp_like),
            )
        con.commit()
        print(f"\nfinalized {len(finalizable)} row(s); stamped {total_unstamped} clip(s) as {OWNER_LICENSE}")
        if ambiguous:
            print(f"{len(ambiguous)} row(s) left unverified — their current decision is not provable legacy state")
    finally:
        con.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
