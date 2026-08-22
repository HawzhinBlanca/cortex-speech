#!/usr/bin/env python3
"""Does every reviewer with a live link actually have work to open?

`supervision-live` asks whether the server is answering. It answers 200 for a reviewer whose queue is
empty, so it cannot see the failure this gate exists for.

MEASURED 2026-08-17, and every layer of it looked correct from the inside:

  * The 1,031 recovered clips were relinked into `D:\\Kurdish Corpora\\sorani\\ZarPodcast`, but
    `dialect.rs` still only knew their pre-recovery path. So all 535 pending Sorani clips were
    UNMAPPED, and the dialect check fails closed — every Sorani-only reviewer had a queue of zero.
  * At the same time the roster file itself was inert: it carried a `"_comment"` string, which a
    strict `HashMap<String, Vec<String>>` parse rejects outright, and the failure path is
    "unrestricted". So the restriction was off for everyone, including the three reviewers who do
    not speak Hawleri.

Either bug alone silently wastes paid reviewer time; together they hid each other. Nothing in the
tree was wrong — the DB rows, the JSON, and the Rust all read correctly in isolation. The only way to
see it is to compute, against the LIVE database and the LIVE roster, what each named reviewer would
actually be served.

Exit 0 = every live-linked reviewer has clips they may judge. Exit 1 = at least one has none.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import sys
from pathlib import Path

# A reviewer this far from running dry is fine; below it, they will be idle within a sitting and the
# owner should know while there is still time to import more of that dialect.
RUNWAY_WARN_CLIPS = 100
VALID_DIALECTS = {"hawleri", "sorani", "badini"}


def _reject_nonfinite(token: str):
    """serde_json rejects NaN/Infinity; Python's json accepts (and json.dumps EMITS) them. A mirror
    that parses what the server refuses reads OK against a dead queue — same rejector the repo
    already uses in bootstrap_legacy_champion and promotion_gate."""
    raise ValueError(f"non-finite JSON token {token!r} (the server rejects this file)")


class PolicyBroken(Exception):
    """A policy file EXISTS but cannot be honoured. Mirror of the server's 503: the queue serves
    NOTHING until the file is fixed (owner instruction 2026-08-20 — present-but-broken fails
    CLOSED), so every live link is effectively dead and the gate must say FAIL, not count clips
    the server will never hand out."""


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def _repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def source_dialects(dialect_rs: str) -> list[tuple[str, str]]:
    """The SOURCE_DIALECTS table, parsed out of dialect.rs.

    Read from the Rust rather than restated here on purpose: a copy in this file is a second source
    of truth that drifts, and drift is precisely the bug — the map said `SoraniVoice_PC_` while the
    clips had moved to `Kurdish Corpora\\sorani`. If the table moves or is renamed this raises, which
    is correct: a gate that silently stops checking anything is worse than one that fails loudly.
    """
    block = re.search(r"const SOURCE_DIALECTS:[^=]*=\s*&\[(.*?)\n\];", dialect_rs, re.S)
    if not block:
        raise AssertionError("could not find SOURCE_DIALECTS in dialect.rs — this gate needs updating")
    # Raw and normal Rust literals are matched SEPARATELY. A raw literal processes no escapes and may
    # end in a backslash (`r"...\sorani\"`), which an escape-aware pattern reads as an escaped quote
    # and then runs straight past the end of the string. Caught by this gate failing against the live
    # machine with the Sorani entries missing from its own table.
    pairs = re.findall(r'\(\s*(?:r"([^"]*)"|"((?:[^"\\]|\\.)*)")\s*,\s*([A-Z_]+)\s*\)', block.group(1))
    consts = dict(re.findall(r'pub const ([A-Z_]+): &str = "([a-z-]+)";', dialect_rs))
    out = []
    for raw_fragment, escaped_fragment, const_name in pairs:
        if const_name not in consts:
            raise AssertionError(f"SOURCE_DIALECTS references unknown constant {const_name}")
        fragment = raw_fragment if raw_fragment else escaped_fragment.replace("\\\\", "\\")
        out.append((fragment, consts[const_name]))
    if not out:
        raise AssertionError("SOURCE_DIALECTS parsed empty — this gate needs updating")
    return out


def dialect_of(audio_path: str, table: list[tuple[str, str]]) -> str | None:
    """Mirror of `dialect::dialect_of`."""
    normalized = audio_path.replace("/", "\\").lower()
    name = normalized.rsplit("\\", 1)[-1]
    for fragment, dialect in table:
        fragment = fragment.lower()
        if fragment in name or fragment in normalized:
            return dialect
    return None


def may_judge(allowed: list[str] | None, audio_path: str, table: list[tuple[str, str]]) -> bool:
    """Mirror of `dialect::reviewer_may_judge`, including its fail-closed unmapped case."""
    if allowed is None:
        return True
    dialect = dialect_of(audio_path, table)
    normalized_allowed = {value.strip().lower() for value in allowed}
    return dialect in normalized_allowed if dialect is not None else False


def load_roster(data_dir: Path) -> dict[str, list[str]]:
    """Mirror of `dialect::load_roster`: missing = unrestricted; present-but-broken raises
    PolicyBroken (the server 503s every queue); `_`-prefixed keys are comments; any other
    non-list-of-strings value is a typo'd RESTRICTION and is broken, not skippable."""
    path = data_dir / "reviewer_dialects.json"
    if not path.is_file():
        return {}
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"), parse_constant=_reject_nonfinite)
    except (OSError, ValueError) as e:
        raise PolicyBroken(f"reviewer_dialects.json is not valid JSON: {e}") from e
    if not isinstance(parsed, dict):
        raise PolicyBroken("reviewer_dialects.json is not a JSON object")
    roster: dict[str, list[str]] = {}
    for name, value in parsed.items():
        if name.startswith("_"):
            continue
        if isinstance(value, list) and all(isinstance(v, str) for v in value):
            if not value:
                raise PolicyBroken(f'reviewer_dialects.json entry "{name}" must name at least one dialect')
            normalized = []
            for dialect in value:
                canonical = dialect.strip().lower()
                if canonical not in VALID_DIALECTS:
                    raise PolicyBroken(
                        f'reviewer_dialects.json entry "{name}" contains unknown dialect "{dialect}" '
                        f'(allowed: {sorted(VALID_DIALECTS)})'
                    )
                if canonical not in normalized:
                    normalized.append(canonical)
            dupe = next((k for k in roster if k.strip().lower() == name.strip().lower()), None)
            if dupe is not None:
                raise PolicyBroken(f'reviewer_dialects.json: "{name}" and "{dupe}" name the same reviewer')
            roster[name] = normalized
        else:
            raise PolicyBroken(f'reviewer_dialects.json entry "{name}" is not a list of dialect names')
    return roster


def allowed_for(roster: dict[str, list[str]], reviewer: str) -> list[str] | None:
    """Mirror of `dialect::allowed_for`: matched the way the session layer matches names (trimmed,
    case-insensitive), never an exact dict lookup — an orphaned roster key silently un-restricted
    exactly the reviewer it named (2026-08-20 hunt)."""
    want = reviewer.strip().lower()
    for name, dialects in roster.items():
        if name.strip().lower() == want:
            return dialects
    return None


def live_reviewers(data_dir: Path, db_path: Path) -> list[str]:
    """The names whose links are live right now, mirroring `couch::load_session`'s OWN authority
    rules (2026-08-20 hunt) — the gate previously read couch_session.json alone and passed a
    red/green verdict about links the server itself considers dead:

      * `couch_session.revoked` is authoritative: Stop writes it FIRST, and its teardown may fail to
        delete the credential file — session present + marker present = NO live links;
      * a session remembered against a DIFFERENT library never resumes, so its reviewers are not
        live against this db_path;
      * an unreadable session file is a question the gate cannot answer — FAIL loudly, never
        "OK (no couch session)" while a running server may be serving from memory.
    """
    session = data_dir / "couch_session.json"
    if (data_dir / "couch_session.revoked").exists():
        return []
    if not session.is_file():
        return []
    try:
        payload = json.loads(session.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise PolicyBroken(f"couch_session.json exists but cannot be read: {e}") from e
    saved_db = payload.get("db_path")
    if isinstance(saved_db, str) and saved_db and Path(saved_db) != db_path:
        return []
    reviewers = payload.get("reviewers")
    if isinstance(reviewers, dict):  # token -> name
        return sorted(set(reviewers.values()))
    if isinstance(reviewers, list):
        return sorted({r for r in reviewers if isinstance(r, str)})
    return []


def load_focus(data_dir: Path) -> set[str] | None:
    """Mirror of `voice_focus::load_focus`: the allow-list of clip ids every queue is narrowed to.

    Same fail-CLOSED contract as the Rust (owner instruction 2026-08-20): missing means no focus;
    present-but-broken raises PolicyBroken, because the server 503s every queue until the file is
    fixed. The gate MUST mirror the server exactly — measured 2026-08-19 the moment the focus went
    live: this gate reported "15,318 servable pending" while every reviewer's real queue held 905.
    A gate that counts clips the server will never hand out reads OK against a dead queue.
    """
    path = data_dir / "voice_focus.json"
    if not path.is_file():
        return None
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"), parse_constant=_reject_nonfinite)
    except (OSError, ValueError) as e:
        raise PolicyBroken(f"voice_focus.json is not valid JSON: {e}") from e
    ids = parsed.get("segment_ids") if isinstance(parsed, dict) else None
    focus = {i for i in ids if isinstance(i, str)} if isinstance(ids, list) else set()
    if not focus:
        raise PolicyBroken("voice_focus.json names no segment ids")
    return focus


def servable_clips(
    db_path: Path, table: list[tuple[str, str]], focus: set[str] | None = None
) -> list[tuple[str, int]]:
    """(audio_path, duration_ms) for every clip the queue would hand out, before dialect.

    The WHERE clause is `db::pending_segment_ids_for`'s, the on-disk check is the one it does per
    distinct path — a row whose audio is gone is not work anybody can do — and `focus` is the
    voice-focus allow-list the server applies after both.
    """
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT id, audio_path, duration_ms FROM speech_segments "
            " WHERE verified = 0 "
            "   AND TRIM(COALESCE(raw_transcript, '')) <> '' "
            "   AND NOT (TRIM(raw_transcript) LIKE '[%]')"
        ).fetchall()
    finally:
        con.close()
    on_disk: dict[str, bool] = {}
    out = []
    for seg_id, path, duration in rows:
        if focus is not None and seg_id not in focus:
            continue
        if path not in on_disk:
            on_disk[path] = os.path.isfile(path)
        if on_disk[path]:
            out.append((path, duration or 0))
    return out


def wrong_dialect_decisions(db_path: Path, roster: dict[str, list[str]], table: list[tuple[str, str]]) -> dict[str, int]:
    """Current, attributed decisions outside the reviewer's present dialect scope.

    ``review_events`` is append-only history, not the authority for the row's current verdict: an
    undo, redo, later reviewer, or owner decision may supersede an older event.  Reading historical
    events here previously blamed an old reviewer even when the segment was now rejected and
    excluded downstream.  ``speech_segments.reviewed_by`` and ``human_decision`` are written
    atomically by the current review path, so they are the only honest current-state attribution.

    This remains a warning rather than a hard failure because tightening a roster can reveal legacy
    work that predates the restriction.  A new warning after activation is nevertheless a routing
    regression that must be investigated.
    """
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT reviewed_by, id, audio_path FROM speech_segments "
            " WHERE verified = 1 "
            "   AND TRIM(COALESCE(reviewed_by, '')) <> '' "
            "   AND LOWER(COALESCE(human_decision, '')) IN "
            "       ('accept','human_accept','edit','human_edit','reject','human_reject')"
        ).fetchall()
    finally:
        con.close()
    offenders: dict[str, int] = {}
    for reviewer, _segment_id, path in rows:
        allowed = allowed_for(roster, reviewer)
        if allowed is not None and not may_judge(allowed, path, table):
            offenders[reviewer] = offenders.get(reviewer, 0) + 1
    return offenders


def evaluate_queues(
    *,
    reviewers: list[str],
    roster: dict[str, list[str]],
    clips: list[tuple[str, int]],
    table: list[tuple[str, str]],
    warn_below: int = RUNWAY_WARN_CLIPS,
) -> tuple[list[str], list[str]]:
    """Pure decision core. Returns (problems, warnings)."""
    problems: list[str] = []
    warnings: list[str] = []
    for who in reviewers:
        allowed = allowed_for(roster, who)
        mine = [(p, d) for p, d in clips if may_judge(allowed, p, table)]
        tag = ", ".join(allowed) if allowed else "unrestricted"
        if not mine:
            problems.append(
                f"{who} ({tag}) has a live link and ZERO clips to review. They will open it, see an "
                f"empty queue, and be unable to work - while being paid."
            )
        elif len(mine) < warn_below:
            hours = sum(d for _, d in mine) / 3_600_000
            warnings.append(f"{who} ({tag}) has only {len(mine)} clips left ({hours:.1f} h) — import more soon.")
    return problems, warnings


def main() -> int:
    data_dir = _data_dir()
    db_path = data_dir / "cortex-speech.db"
    if not db_path.is_file():
        print(f"REVIEWER QUEUES: SKIP-ENV (no library at {db_path})", flush=True)
        return 0

    try:
        reviewers = live_reviewers(data_dir, db_path)
    except PolicyBroken as e:
        print("REVIEWER QUEUES: FAIL", flush=True)
        print(f"  - {e} — cannot tell which links are live; a running server may be serving from memory", flush=True)
        return 1
    if not reviewers:
        print("REVIEWER QUEUES: OK (no couch session — no links are live)", flush=True)
        return 0

    table = source_dialects((_repo_root() / "src-tauri" / "src" / "dialect.rs").read_text(encoding="utf-8"))
    try:
        roster = load_roster(data_dir)
        focus = load_focus(data_dir)
    except PolicyBroken as e:
        # The server 503s every queue while a policy file is broken, so every live link is dead.
        print("REVIEWER QUEUES: FAIL", flush=True)
        print(f"  - {e} — the server serves NOTHING to any reviewer until this file is fixed", flush=True)
        return 1
    clips = servable_clips(db_path, table, focus)
    if focus is not None:
        print(f"voice focus ACTIVE: queues narrowed to {len(focus)} clip id(s)", flush=True)

    problems, warnings = evaluate_queues(reviewers=reviewers, roster=roster, clips=clips, table=table)

    offenders = wrong_dialect_decisions(db_path, roster, table)
    if offenders:
        total = sum(offenders.values())
        detail = ", ".join(f"{who} {n}" for who, n in sorted(offenders.items()))
        warnings.append(
            f"{total} current attributed decision(s) are outside the reviewer's present dialect scope "
            f"({detail}). Accepted/edited rows may influence downstream data; rejected rows remain "
            f"excluded but still indicate a routing-policy breach. Investigate before export."
        )

    for w in warnings:
        print(f"  ! {w}", flush=True)
    if problems:
        print("REVIEWER QUEUES: FAIL", flush=True)
        for p in problems:
            print(f"  - {p}", flush=True)
        by_dialect: dict[str | None, int] = {}
        for path, _ in clips:
            key = dialect_of(path, table)
            by_dialect[key] = by_dialect.get(key, 0) + 1
        print(f"  servable pending clips by dialect: {by_dialect}", flush=True)
        return 1

    print(
        f"REVIEWER QUEUES: OK ({len(reviewers)} live link(s), every one has clips to review; "
        f"{len(clips)} servable pending)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
