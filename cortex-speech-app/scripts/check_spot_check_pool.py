"""Gate: the hidden spot-check mechanism must be ABLE TO FIRE, on the live database.

Spot checks are the only instrument that measures whether a reviewer is LISTENING rather than
tapping "looks good". They work by re-serving a clip whose correct answer is already known and
scoring what comes back. That requires ANSWER KEYS, and `Database::list_spot_check_candidates`
mints them from exactly one population:

    verified = 1  AND  raw_transcript <> ''  AND  (is_gold = 1 OR reviewed_by IS NULL)
    AND  quality::human_verified_text(seg) is Some   (a REAL human decision produced the text)
    AND  learning_key(answer) != learning_key(raw)   (a draft already correct catches nobody)

MEASURED 2026-08-13: that pool was **0** while the corpus held 288 human decisions. Every decision
had arrived from the phone, and the phone stamps `reviewed_by` unconditionally — deliberately, since
one reviewer's fresh correction must never become the key another reviewer is graded against. With
nothing flagged `is_gold` either, the population was empty by construction. Nothing errored, no
counter moved, and `spot_checks` still showed 5 rows from an earlier era: the QC looked alive while
being structurally incapable of firing. That is the vacuous-gate class this repo keeps finding, and
it had been silently true across 288 decisions.

An empty pool is therefore a FAILURE, not a quiet zero. Fixing it is an owner action, not a code
change: adjudicate clips at the DESKTOP (which leaves `reviewed_by` NULL) or flag adjudicated clips
`is_gold = 1`, so keys exist to grade against.

Run:  python scripts/check_spot_check_pool.py [db_path]   (env CORTEX_DB overrides)
"""

import os
import sqlite3
import sys

# Keys per active reviewer below which the measurement is too thin to mean anything. The research
# basis for hidden-gold density is ~10-15% of served work; this floor is deliberately far lower,
# because the purpose here is to catch STRUCTURAL death (a pool of zero), not to tune density.
MIN_KEYS_PER_REVIEWER = 3

HUMAN_DECIDED = (
    "(LOWER(COALESCE(human_decision,'')) IN ('accept','edit','human_accept','human_edit')"
    " OR LOWER(COALESCE(verdict,'')) IN ('human_accept','human_edit'))"
)


def default_db_path() -> str:
    return os.environ.get("CORTEX_DB") or os.path.join(os.environ["APPDATA"], "cortex-speech", "cortex-speech.db")


def main() -> int:
    db_path = sys.argv[1] if len(sys.argv) > 1 else default_db_path()
    if not os.path.exists(db_path):
        print(f"FAIL: database not found: {db_path}")
        return 1
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)

    reviewers = conn.execute("SELECT COUNT(DISTINCT reviewer) FROM review_events").fetchone()[0]
    decisions = conn.execute(
        f"SELECT COUNT(*) FROM speech_segments WHERE verified = 1 AND {HUMAN_DECIDED}"
    ).fetchone()[0]

    if decisions == 0:
        print("SKIP: no human decisions yet — the spot-check pool cannot be judged before review starts")
        return 0

    # Mirrors list_spot_check_candidates + human_verified_text + the wrong-draft requirement.
    keys = conn.execute(
        f"""
        SELECT COUNT(*) FROM speech_segments
        WHERE verified = 1
          AND TRIM(COALESCE(raw_transcript,'')) <> ''
          AND (is_gold = 1 OR reviewed_by IS NULL)
          AND {HUMAN_DECIDED}
          AND TRIM(COALESCE(NULLIF(verdict_transcript,''), NULLIF(annotated_transcript,''), raw_transcript))
              <> TRIM(raw_transcript)
        """
    ).fetchone()[0]

    needed = max(MIN_KEYS_PER_REVIEWER * max(reviewers, 1), MIN_KEYS_PER_REVIEWER)
    print(f"human decisions        : {decisions}")
    print(f"active reviewers       : {reviewers}")
    print(f"spot-check answer keys : {keys}   (need >= {needed})")

    if keys == 0:
        print("FAIL: ZERO answer keys — the spot-check QC cannot fire at all.")
        print("      Every decision so far has been made with NO listening check behind it.")
        print("      Fix (owner action): adjudicate clips at the DESKTOP (leaves reviewed_by NULL),")
        print("      or flag adjudicated clips is_gold = 1, so keys exist to grade against.")
        return 1
    if keys < needed:
        print(f"FAIL: only {keys} answer keys for {reviewers} reviewer(s) — too thin to measure attention.")
        return 1

    print("SPOT-CHECK POOL: healthy — the QC that proves reviewers are listening can fire")
    return 0


if __name__ == "__main__":
    sys.exit(main())
