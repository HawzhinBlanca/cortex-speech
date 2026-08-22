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
import json
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path

from pilot_focus_contract import verify_controlled_pilot_focus

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
REVIEW_PILOT_FILE = "review_pilot_policy.json"
COUCH_SESSION_FILE = "couch_session.json"
REVIEW_PILOT_SCHEMA_VERSION = 1
REVIEW_PILOT_REVIEWERS = 2
REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER = 10
REVIEW_PILOT_TOTAL_CORPUS_ACTIONS = 20

HUMAN_DECIDED = """(
    CASE WHEN TRIM(COALESCE(human_decision,'')) <> ''
         THEN LOWER(human_decision) IN ('accept','edit','human_accept','human_edit')
         ELSE LOWER(COALESCE(verdict,'')) IN ('human_accept','human_edit')
    END
)"""


@dataclass(frozen=True)
class ReviewPilotPolicy:
    after_review_event_id: int
    max_total_corpus_actions: int
    reviewer_caps: dict[str, int]


def _strict_json_loads(raw: str) -> object:
    """Match serde_json: reject duplicate object fields and non-JSON numeric constants."""
    def object_without_duplicates(pairs: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON field {key!r}")
            value[key] = item
        return value

    def invalid_constant(value: str) -> object:
        raise ValueError(f"invalid JSON constant {value}")

    return json.loads(raw, object_pairs_hook=object_without_duplicates, parse_constant=invalid_constant)


def load_review_pilot_policy(data_dir: Path) -> ReviewPilotPolicy | None:
    """Mirror Rust's strict two-reviewer/20-action operating-policy contract."""
    path = data_dir / REVIEW_PILOT_FILE
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except OSError as error:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} is unreadable: {error}") from error
    try:
        value = _strict_json_loads(raw)
    except (json.JSONDecodeError, ValueError) as error:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} is invalid JSON: {error}") from error
    if not isinstance(value, dict) or set(value) != {
        "schema_version",
        "after_review_event_id",
        "max_total_corpus_actions",
        "reviewers",
    }:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} has missing or unknown fields")
    if type(value["schema_version"]) is not int or value["schema_version"] != REVIEW_PILOT_SCHEMA_VERSION:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} schema_version must be {REVIEW_PILOT_SCHEMA_VERSION}")
    after = value["after_review_event_id"]
    total = value["max_total_corpus_actions"]
    if isinstance(after, bool) or not isinstance(after, int) or after < 0:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} after_review_event_id must be non-negative")
    if isinstance(total, bool) or total != REVIEW_PILOT_TOTAL_CORPUS_ACTIONS:
        raise PolicyBroken(
            f"{REVIEW_PILOT_FILE} must cap exactly {REVIEW_PILOT_TOTAL_CORPUS_ACTIONS} corpus actions"
        )
    entries = value["reviewers"]
    if not isinstance(entries, list) or len(entries) != REVIEW_PILOT_REVIEWERS:
        raise PolicyBroken(f"{REVIEW_PILOT_FILE} must name exactly {REVIEW_PILOT_REVIEWERS} reviewers")
    caps: dict[str, int] = {}
    normalized: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"name", "max_corpus_actions"}:
            raise PolicyBroken(f"{REVIEW_PILOT_FILE} has an invalid reviewer entry")
        name = entry["name"]
        cap = entry["max_corpus_actions"]
        if not isinstance(name, str) or not name.strip() or len(name.strip()) > 40 or any(
            ord(char) < 32 or ord(char) == 127 for char in name
        ):
            raise PolicyBroken(f"{REVIEW_PILOT_FILE} has an invalid reviewer name")
        name = name.strip()
        key = name.lower()
        if key in normalized:
            raise PolicyBroken(f"{REVIEW_PILOT_FILE} reviewer names must be distinct")
        if isinstance(cap, bool) or cap != REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER:
            raise PolicyBroken(
                f"{REVIEW_PILOT_FILE} must cap each reviewer at {REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER} corpus actions"
            )
        normalized.add(key)
        caps[name] = cap
    policy = ReviewPilotPolicy(after, total, dict(sorted(caps.items(), key=lambda item: item[0].lower())))
    try:
        verify_controlled_pilot_focus(data_dir)
    except RuntimeError as error:
        raise PolicyBroken(str(error)) from error
    return policy


def load_pilot_served_checks(
    data_dir: Path, policy: ReviewPilotPolicy
) -> dict[str, set[str]]:
    """Read the durable distinct-key budget bound into the remembered Couch session."""
    session_path = data_dir / COUCH_SESSION_FILE
    try:
        session = json.loads(session_path.read_text(encoding="utf-8"))
        policy_json = json.loads((data_dir / REVIEW_PILOT_FILE).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyBroken(f"controlled-pilot session state is unreadable: {error}") from error
    if not isinstance(session, dict) or session.get("pilot_policy") != policy_json:
        raise PolicyBroken("remembered session is not bound to the active controlled-pilot policy")
    entries = session.get("pilot_spot_checks")
    if not isinstance(entries, list):
        raise PolicyBroken("remembered session has no valid pilot hidden-check budget")
    out = {name: set() for name in policy.reviewer_caps}
    for entry in entries:
        if (
            not isinstance(entry, list)
            or len(entry) != 2
            or not all(isinstance(value, str) and value.strip() for value in entry)
        ):
            raise PolicyBroken("remembered session has an invalid pilot hidden-check entry")
        segment_id, actual = entry
        canonical = next((name for name in out if name.lower() == actual.strip().lower()), None)
        if canonical is None:
            raise PolicyBroken(f"pilot hidden-check budget contains unauthorized reviewer {actual!r}")
        out[canonical].add(segment_id)
    return out


def pilot_progress(
    conn: sqlite3.Connection, policy: ReviewPilotPolicy
) -> tuple[int, dict[str, int]]:
    """Count exactly the real Couch events the transactional server cap counts."""
    current_max = conn.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0]
    if policy.after_review_event_id > current_max:
        raise PolicyBroken("controlled-review baseline is ahead of durable review history")
    counts = {name: 0 for name in policy.reviewer_caps}
    total = 0
    for actual, count in conn.execute(
        """
        SELECT reviewer, COUNT(*) FROM review_events
        WHERE id > ? AND source = 'couch' AND action IN ('accept','edit','reject','skip')
        GROUP BY LOWER(TRIM(reviewer))
        """,
        (policy.after_review_event_id,),
    ):
        canonical = next((name for name in counts if name.lower() == actual.strip().lower()), None)
        if canonical is None:
            raise PolicyBroken(f"controlled-review history contains unauthorized reviewer {actual!r}")
        if count < 0 or count > policy.reviewer_caps[canonical]:
            raise PolicyBroken(f"controlled-review history exceeds the limit for {canonical}")
        counts[canonical] = count
        total += count
    if total > policy.max_total_corpus_actions:
        raise PolicyBroken("controlled-review history exceeds the total limit")
    return total, counts


def pilot_bounded_work_counts(
    work_counts: dict[str, int], policy: ReviewPilotPolicy, progress: dict[str, int]
) -> dict[str, int]:
    """Future work the server can still authorize, never the full accessible corpus."""
    out: dict[str, int] = {}
    for actual, accessible in work_counts.items():
        canonical = next(name for name in policy.reviewer_caps if name.lower() == actual.strip().lower())
        out[actual] = min(accessible, policy.reviewer_caps[canonical] - progress[canonical])
    return out


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


def pilot_required_fresh_keys(
    action_cap: int,
    remaining_work: int,
    distinct_served: int,
    queue_batch: int,
    spot_check_every: int,
    *,
    at_action_cap: bool = False,
) -> int:
    """Fresh keys the bounded server may still mint; the ordinary three-key floor does not apply."""
    if action_cap <= 0 or distinct_served < 0 or distinct_served > action_cap:
        raise ValueError("pilot action cap/served count is invalid")
    quota = (action_cap + spot_check_every - 1) // spot_check_every
    missing = max(0, quota - distinct_served)
    if at_action_cap:
        return missing
    return min(missing, required_keys_for_work(remaining_work, queue_batch, spot_check_every))


def pilot_certification_issues(
    conn: sqlite3.Connection,
    policy: ReviewPilotPolicy,
    progress: dict[str, int],
    served: dict[str, set[str]],
    spot_check_every: int,
) -> list[str]:
    """Strict canary result: no skip and, at cap, two exact hidden answers per reviewer."""
    issues: list[str] = []
    for reviewer, cap in policy.reviewer_caps.items():
        skip_count = int(
            conn.execute(
                """
                SELECT COUNT(*) FROM review_events
                 WHERE id > ? AND source = 'couch' AND action = 'skip'
                   AND LOWER(TRIM(reviewer)) = ?
                """,
                (policy.after_review_event_id, reviewer.lower()),
            ).fetchone()[0]
        )
        if skip_count:
            issues.append(
                f"{reviewer} used {skip_count} skip action(s); the 10-corpus-decision canary is incomplete"
            )
        if progress[reviewer] < cap:
            continue
        quota = (cap + spot_check_every - 1) // spot_check_every
        ids = served[reviewer]
        if len(ids) != quota:
            issues.append(f"{reviewer} reached the corpus cap with {len(ids)}/{quota} pilot keys served")
        results = {
            segment_id: (int(noticed), float(cer))
            for segment_id, noticed, cer in conn.execute(
                "SELECT segment_id, noticed, cer FROM spot_checks WHERE LOWER(TRIM(reviewer)) = ?",
                (reviewer.lower(),),
            ).fetchall()
            if segment_id in ids
        }
        if len(results) != quota:
            issues.append(f"{reviewer} reached the corpus cap with {len(results)}/{quota} pilot results")
            continue
        failed = [segment_id for segment_id, (noticed, cer) in results.items() if noticed != 1 or abs(cer) > 1e-12]
        if failed:
            issues.append(
                f"{reviewer} failed {len(failed)}/{quota} pilot checks; certification requires 2/2 noticed at CER 0"
            )
    return issues


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
    pilot_served: dict[str, set[str]] | None = None
    try:
        roster = load_roster(data_dir)
        focus = load_focus(data_dir)
        reviewers = live_reviewers(data_dir, Path(db_path))
        pilot_policy = load_review_pilot_policy(data_dir)
        if pilot_policy is not None:
            live = {name.strip().lower() for name in reviewers}
            configured = {name.strip().lower() for name in pilot_policy.reviewer_caps}
            if live != configured or len(reviewers) != REVIEW_PILOT_REVIEWERS:
                raise PolicyBroken(
                    f"{REVIEW_PILOT_FILE} requires exactly these live reviewers: "
                    + ", ".join(pilot_policy.reviewer_caps)
                )
            pilot_served = load_pilot_served_checks(data_dir, pilot_policy)
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
    pilot_total = None
    pilot_counts = None
    certification_issues: list[str] = []
    if pilot_policy is not None:
        try:
            pilot_total, pilot_counts = pilot_progress(conn, pilot_policy)
        except PolicyBroken as error:
            conn.close()
            print(f"FAIL: reviewer pilot policy cannot be evaluated: {error}")
            return 1
        work_counts = pilot_bounded_work_counts(work_counts, pilot_policy, pilot_counts)
        assert pilot_served is not None
        declined = {
            (segment_id, reviewer.strip().lower())
            for segment_id, reviewer in conn.execute(
                """
                SELECT segment_id, reviewer FROM review_events
                 WHERE id > ? AND source = 'couch' AND action = 'skip'
                """,
                (pilot_policy.after_review_event_id,),
            ).fetchall()
        }
        for canonical, ids in pilot_served.items():
            quota = (pilot_policy.reviewer_caps[canonical] + spot_check_every - 1) // spot_check_every
            if len(ids) > quota:
                conn.close()
                print(f"FAIL: {canonical} has {len(ids)} pilot keys served beyond the {quota}-key ceiling")
                return 1
            unresolved = {
                segment_id
                for segment_id in ids
                if (segment_id, canonical.lower()) not in already_scored
                and (segment_id, canonical.lower()) not in declined
            }
            if unresolved:
                valid = available_keys_by_reviewer(
                    reviewers=[canonical],
                    roster=roster,
                    focus=focus,
                    candidates=[candidate for candidate in candidates if candidate[0] in unresolved],
                    already_scored=set(),
                    dialect_table=dialect_table,
                )[canonical]
                if valid != len(unresolved):
                    conn.close()
                    print(f"FAIL: {canonical} has a previously served pilot key that is no longer valid")
                    return 1
        # The ordinary candidate query still includes served-but-unanswered keys.  They will be
        # re-served, not minted again, so exclude them from FRESH capacity just as the Rust planner does.
        served_as_scored = {
            (segment_id, canonical.lower())
            for canonical, ids in pilot_served.items()
            for segment_id in ids
        }
        counts = available_keys_by_reviewer(
            reviewers=reviewers,
            roster=roster,
            focus=focus,
            candidates=candidates,
            already_scored=already_scored | served_as_scored,
            dialect_table=dialect_table,
        )
        certification_issues = pilot_certification_issues(
            conn, pilot_policy, pilot_counts, pilot_served, spot_check_every
        )
    conn.close()
    required: dict[str, int] = {}
    for reviewer in reviewers:
        if pilot_policy is None:
            required[reviewer] = max(
                MIN_KEYS_PER_REVIEWER,
                required_keys_for_work(work_counts[reviewer], queue_batch, spot_check_every),
            )
            continue
        assert pilot_served is not None
        canonical = next(name for name in pilot_policy.reviewer_caps if name.lower() == reviewer.strip().lower())
        required[reviewer] = pilot_required_fresh_keys(
            pilot_policy.reviewer_caps[canonical],
            work_counts[reviewer],
            len(pilot_served[canonical]),
            queue_batch,
            spot_check_every,
            at_action_cap=pilot_counts is not None
            and pilot_counts[canonical] >= pilot_policy.reviewer_caps[canonical],
        )

    print(f"human decisions        : {decisions}")
    print(f"active reviewers       : {len(reviewers)}")
    print(f"voice focus            : {'active' if focus is not None else 'none'}")
    print(f"serving cadence        : {queue_batch} work / 1 check every {spot_check_every} (rounded per refill)")
    if pilot_policy is not None and pilot_total is not None and pilot_counts is not None:
        print(
            f"controlled pilot       : {pilot_total}/{pilot_policy.max_total_corpus_actions} corpus actions "
            f"(+ exactly 2 hidden QC per reviewer, max 24 compensated UI acts); "
            + ", ".join(
                f"{name}={pilot_counts[name]}/{pilot_policy.reviewer_caps[name]}"
                for name in pilot_policy.reviewer_caps
            )
        )
    for reviewer in reviewers:
        print(
            f"  {reviewer}: {counts[reviewer]} fresh available key(s) / {required[reviewer]} required "
            f"for {work_counts[reviewer]} accessible work clip(s)"
        )

    insufficient = {name: (counts[name], required[name]) for name in reviewers if counts[name] < required[name]}
    if insufficient:
        details = ", ".join(f"{name}={available}/{needed}" for name, (available, needed) in insufficient.items())
        print(f"FAIL: hidden-check capacity cannot cover the accessible paid-review campaign: {details}")
        print("      Add owner-adjudicated/is_gold keys inside the active focus and each reviewer's dialect,")
        print("      or narrow the campaign/roster before opening links. Never fabricate answer keys.")
        if pilot_policy is not None:
            print("      Requirements derive from the server-enforced 10-action/two-key cap for each pilot reviewer.")
        else:
            print("      Requirements are per reviewer because no enforced work quota prevents one eligible")
            print("      reviewer from draining the entire accessible queue.")
        return 1

    if certification_issues:
        print("FAIL: controlled pilot is not certified:")
        for issue in certification_issues:
            print(f"      - {issue}")
        print("      At the action cap, Couch serves hidden-only catch-up; no third key or corpus action is allowed.")
        return 1

    print("SPOT-CHECK POOL: healthy — fresh hidden checks cover every live reviewer's accessible campaign")
    return 0


if __name__ == "__main__":
    sys.exit(main())
