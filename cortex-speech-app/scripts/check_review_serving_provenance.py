"""Gate: machine text must never sit in the human-only field, and untouched clips must serve champion text.

Why this gate exists (incident, 2026-08-12): the phone review server's ``review_text()`` rightly
prefers ``annotated_transcript`` — the field reserved for a HUMAN's typed correction — over the raw
champion draft. Machine writers had filled that field on 348 rows (the batch persist path seeded it
via ``COALESCE(annotated_transcript, machine_draft)`` — a LIVE writer at the time, removed and
pinned by ``test_machine_never_writes_annotated_policy.py``; the desktop re-transcribe handlers did
the same), so reviewers were served a stale machine paraphrase while every fresh champion +
refinement draft sat invisible in ``raw_transcript`` / ``normalized_transcript``. Words the speaker
never said reached human reviewers labeled as the draft.

The law this gate enforces, on the LIVE database, at the serving path's own precedence:

  1. ``annotated_transcript`` is human-only: a non-empty value requires human evidence on that row
     (a review event, a recorded human_decision, or verified=1).
  2. Every clip with NO human evidence must fall back to text the champion actually produced:
     its ``raw_transcript`` must byte-match (modulo surrounding whitespace) the stored
     ``omniasr-wsl-7b`` hypothesis for that segment. No champion hypothesis on file = FAIL
     (hard-stop law: an undrafted clip may not sit in the queue looking drafted).

Run:  python scripts/check_review_serving_provenance.py [db_path]
      (defaults to %APPDATA%/cortex-speech/cortex-speech.db, override with CORTEX_DB)
"""

import os
import sqlite3
import sys

CHAMPION_MODEL_ID = "omniasr-wsl-7b"


def default_db_path() -> str:
    override = os.environ.get("CORTEX_DB")
    if override:
        return override
    return os.path.join(os.environ["APPDATA"], "cortex-speech", "cortex-speech.db")


def human_field_violations(conn: sqlite3.Connection) -> list:
    """Rows where the human-only field is populated with no human evidence anywhere."""
    return conn.execute(
        """
        SELECT s.id
        FROM speech_segments s
        WHERE TRIM(COALESCE(s.annotated_transcript, '')) <> ''
          AND COALESCE(s.human_decision, '') = ''
          AND s.verified = 0
          AND NOT EXISTS (SELECT 1 FROM review_events e WHERE e.segment_id = s.id)
          AND NOT EXISTS (
                SELECT 1 FROM decision_log d
                WHERE d.segment_id = s.id AND COALESCE(d.human_decision, '') <> ''
          )
        ORDER BY s.id
        """
    ).fetchall()


def non_champion_fallbacks(conn: sqlite3.Connection) -> list:
    """Untouched rows whose served fallback text is not the champion's own output."""
    rows = conn.execute(
        """
        SELECT s.id, s.raw_transcript,
               (SELECT h.transcript FROM segment_hypotheses h
                WHERE h.segment_id = s.id AND h.model_id = ?) AS champion
        FROM speech_segments s
        WHERE COALESCE(s.human_decision, '') = ''
          AND s.verified = 0
          AND NOT EXISTS (SELECT 1 FROM review_events e WHERE e.segment_id = s.id)
          AND NOT EXISTS (
                SELECT 1 FROM decision_log d
                WHERE d.segment_id = s.id AND COALESCE(d.human_decision, '') <> ''
          )
        """,
        (CHAMPION_MODEL_ID,),
    ).fetchall()
    bad = []
    for seg_id, raw, champion in rows:
        if champion is None or (raw or "").strip() != champion.strip():
            bad.append((seg_id, "no champion hypothesis" if champion is None else "raw != champion"))
    return bad


def main() -> int:
    db_path = sys.argv[1] if len(sys.argv) > 1 else default_db_path()
    if not os.path.exists(db_path):
        print(f"FAIL: database not found: {db_path}")
        return 1
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)

    failures = 0

    ghosts = human_field_violations(conn)
    if ghosts:
        failures += 1
        print(f"FAIL [human-field]: {len(ghosts)} rows hold text in annotated_transcript with no human evidence")
        for (seg_id,) in ghosts[:10]:
            print(f"  {seg_id}")
        if len(ghosts) > 10:
            print(f"  ... and {len(ghosts) - 10} more")
    else:
        print("PASS [human-field]: every populated annotated_transcript has human evidence")

    drifted = non_champion_fallbacks(conn)
    if drifted:
        failures += 1
        print(f"FAIL [champion-fallback]: {len(drifted)} untouched rows would serve non-champion text")
        for seg_id, why in drifted[:10]:
            print(f"  {seg_id}: {why}")
        if len(drifted) > 10:
            print(f"  ... and {len(drifted) - 10} more")
    else:
        print("PASS [champion-fallback]: every untouched row serves the champion's own transcript")

    if failures:
        print(f"REVIEW SERVING PROVENANCE: {failures} invariant(s) violated")
        return 1
    print("REVIEW SERVING PROVENANCE: all invariants hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
