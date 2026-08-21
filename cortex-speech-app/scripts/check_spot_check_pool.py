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
from pathlib import Path

from check_reviewer_queues_live import (
    PolicyBroken,
    allowed_for,
    live_reviewers,
    load_focus,
    load_roster,
    may_judge,
    servable_clips,
    source_dialects,
)

# Absolute floor for a live reviewer, even when another gate already says their queue is empty.
# Production capacity is stricter and is derived below from the reviewer's whole accessible queue.
MIN_KEYS_PER_REVIEWER = 3

HUMAN_DECIDED = """(
    CASE WHEN TRIM(COALESCE(human_decision,'')) <> ''
         THEN LOWER(human_decision) IN ('accept','edit','human_accept','human_edit')
         ELSE LOWER(COALESCE(verdict,'')) IN ('human_accept','human_edit')
    END
)"""


def default_db_path() -> str:
    return os.environ.get("CORTEX_DB") or os.path.join(os.environ["APPDATA"], "cortex-speech", "cortex-speech.db")


def learning_key(text: str) -> str:
    """Mirror `normalizer::learning_text_key`: lowercase plus whitespace collapse only."""
    return " ".join((text or "").lower().split())


def serving_constants(couch_rs: str) -> tuple[int, int]:
    """Read queue/check cadence from the serving implementation, never a second copied policy."""
    import re

    values: dict[str, int] = {}
    for name in ("QUEUE_BATCH", "SPOT_CHECK_EVERY"):
        match = re.search(rf"const\s+{name}:\s*usize\s*=\s*(\d+)\s*;", couch_rs)
        if not match:
            raise AssertionError(f"could not resolve {name} from couch.rs — this gate needs updating")
        values[name] = int(match.group(1))
    if values["QUEUE_BATCH"] <= 0 or values["SPOT_CHECK_EVERY"] <= 0:
        raise AssertionError("queue and spot-check cadence must be positive")
    return values["QUEUE_BATCH"], values["SPOT_CHECK_EVERY"]


def required_keys_for_work(work_clips: int, queue_batch: int, spot_check_every: int) -> int:
    """Fresh keys one reviewer needs to finish all work they are currently allowed to receive.

    The server rounds checks UP separately on every queue refill. For example, 26 work clips are a
    full 25-item refill (4 checks) plus a one-item refill (1 check), hence 5 rather than ceil(26/8).
    No reviewer quota exists, so assuming an even split would be an unenforced promise; the safe
    bound is that any eligible reviewer may be the person who drains the accessible campaign.
    """
    if work_clips < 0 or queue_batch <= 0 or spot_check_every <= 0:
        raise ValueError("work and serving cadence must be non-negative/positive")
    full_batches, remainder = divmod(work_clips, queue_batch)
    checks_per_full_batch = (queue_batch + spot_check_every - 1) // spot_check_every
    remainder_checks = (remainder + spot_check_every - 1) // spot_check_every if remainder else 0
    return full_batches * checks_per_full_batch + remainder_checks


def work_counts_by_reviewer(
    *,
    reviewers: list[str],
    roster: dict[str, list[str]],
    clips: list[tuple[str, int]],
    dialect_table: list[tuple[str, str]],
) -> dict[str, int]:
    """Count the currently servable work each reviewer is permitted to consume."""
    return {
        reviewer: sum(
            1 for audio_path, _duration_ms in clips if may_judge(allowed_for(roster, reviewer), audio_path, dialect_table)
        )
        for reviewer in reviewers
    }


def available_keys_by_reviewer(
    *,
    reviewers: list[str],
    roster: dict[str, list[str]],
    focus: set[str] | None,
    candidates: list[tuple[str, str, str, str]],
    already_scored: set[tuple[str, str]],
    dialect_table: list[tuple[str, str]],
) -> dict[str, int]:
    """Count keys the Rust selector can actually serve to each reviewer.

    Candidate tuples are `(segment_id, audio_path, raw, expected)`. Filtering happens before the
    count, matching `list_spot_check_candidates`: current voice focus, on-disk audio, reviewer
    dialect, genuine wrong draft, and not previously scored by this reviewer.
    """
    out: dict[str, int] = {}
    for reviewer in reviewers:
        allowed = allowed_for(roster, reviewer)
        reviewer_key = reviewer.strip().lower()
        count = 0
        for segment_id, audio_path, raw, expected in candidates:
            if focus is not None and segment_id not in focus:
                continue
            if (segment_id, reviewer_key) in already_scored:
                continue
            if not os.path.isfile(audio_path):
                continue
            if not may_judge(allowed, audio_path, dialect_table):
                continue
            if learning_key(expected) == learning_key(raw):
                continue
            count += 1
        out[reviewer] = count
    return out


def main() -> int:
    db_path = sys.argv[1] if len(sys.argv) > 1 else default_db_path()
    if not os.path.exists(db_path):
        print(f"FAIL: database not found: {db_path}")
        return 1
    data_dir = Path(db_path).parent
    try:
        roster = load_roster(data_dir)
        focus = load_focus(data_dir)
        reviewers = live_reviewers(data_dir, Path(db_path))
        dialect_rs = (Path(__file__).resolve().parent.parent / "src-tauri" / "src" / "dialect.rs").read_text(
            encoding="utf-8"
        )
        dialect_table = source_dialects(dialect_rs)
        couch_rs = (Path(__file__).resolve().parent.parent / "src-tauri" / "src" / "couch.rs").read_text(
            encoding="utf-8"
        )
        queue_batch, spot_check_every = serving_constants(couch_rs)
    except (PolicyBroken, OSError, AssertionError) as error:
        print(f"FAIL: reviewer policy cannot be evaluated: {error}")
        return 1

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)

    decisions = conn.execute(
        f"SELECT COUNT(*) FROM speech_segments WHERE verified = 1 AND {HUMAN_DECIDED}"
    ).fetchone()[0]

    if decisions == 0:
        if reviewers:
            print("FAIL: live reviewer links exist but there are no human answer keys for hidden listening checks")
            return 1
        print("SKIP: no human decisions and no live reviewer session")
        return 0

    if not reviewers:
        print("SKIP: no live reviewer session — spot-check capacity is a launch-time gate")
        return 0

    # Mirrors the SQL half of list_spot_check_candidates + human_verified_text. The remaining
    # per-reviewer/focus/on-disk/learning-key filters run in `available_keys_by_reviewer` below.
    candidates = conn.execute(
        f"""
        SELECT id, audio_path, raw_transcript,
               COALESCE(NULLIF(verdict_transcript,''), NULLIF(annotated_transcript,''), raw_transcript)
          FROM speech_segments
        WHERE verified = 1
          AND TRIM(COALESCE(raw_transcript,'')) <> ''
          AND (is_gold = 1 OR reviewed_by IS NULL)
          AND {HUMAN_DECIDED}
        """
    ).fetchall()
    already_scored = {
        (segment_id, reviewer.strip().lower())
        for segment_id, reviewer in conn.execute("SELECT segment_id, reviewer FROM spot_checks").fetchall()
    }
    conn.close()
    counts = available_keys_by_reviewer(
        reviewers=reviewers,
        roster=roster,
        focus=focus,
        candidates=candidates,
        already_scored=already_scored,
        dialect_table=dialect_table,
    )
    work = servable_clips(Path(db_path), dialect_table, focus)
    work_counts = work_counts_by_reviewer(
        reviewers=reviewers,
        roster=roster,
        clips=work,
        dialect_table=dialect_table,
    )
    required = {
        reviewer: max(
            MIN_KEYS_PER_REVIEWER,
            required_keys_for_work(work_counts[reviewer], queue_batch, spot_check_every),
        )
        for reviewer in reviewers
    }

    print(f"human decisions        : {decisions}")
    print(f"active reviewers       : {len(reviewers)}")
    print(f"voice focus            : {'active' if focus is not None else 'none'}")
    print(f"serving cadence        : {queue_batch} work / 1 check every {spot_check_every} (rounded per refill)")
    for reviewer in reviewers:
        print(
            f"  {reviewer}: {counts[reviewer]} available key(s) / {required[reviewer]} required "
            f"for {work_counts[reviewer]} accessible work clip(s)"
        )

    insufficient = {name: (counts[name], required[name]) for name in reviewers if counts[name] < required[name]}
    if insufficient:
        details = ", ".join(f"{name}={available}/{needed}" for name, (available, needed) in insufficient.items())
        print(f"FAIL: hidden-check capacity cannot cover the accessible paid-review campaign: {details}")
        print("      Add owner-adjudicated/is_gold keys inside the active focus and each reviewer's dialect,")
        print("      or narrow the campaign/roster before opening links. Never fabricate answer keys.")
        print("      Requirements are per reviewer because no enforced work quota prevents one eligible")
        print("      reviewer from draining the entire accessible queue.")
        return 1

    print("SPOT-CHECK POOL: healthy — fresh hidden checks cover every live reviewer's accessible campaign")
    return 0


if __name__ == "__main__":
    sys.exit(main())
