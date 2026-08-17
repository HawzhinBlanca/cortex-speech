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
    normalized = audio_path.replace("/", "\\")
    name = normalized.rsplit("\\", 1)[-1]
    for fragment, dialect in table:
        if fragment in name or fragment in normalized:
            return dialect
    return None


def may_judge(allowed: list[str] | None, audio_path: str, table: list[tuple[str, str]]) -> bool:
    """Mirror of `dialect::reviewer_may_judge`, including its fail-closed unmapped case."""
    if allowed is None:
        return True
    dialect = dialect_of(audio_path, table)
    return dialect in allowed if dialect is not None else False


def load_roster(data_dir: Path) -> dict[str, list[str]]:
    """Mirror of `dialect::load_roster`: array-of-strings values are entries, anything else is not."""
    path = data_dir / "reviewer_dialects.json"
    if not path.is_file():
        return {}
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    if not isinstance(parsed, dict):
        return {}
    return {
        name: value
        for name, value in parsed.items()
        if isinstance(value, list) and all(isinstance(v, str) for v in value)
    }


def live_reviewers(data_dir: Path) -> list[str]:
    """The names whose links are live right now, from the remembered couch session."""
    session = data_dir / "couch_session.json"
    if not session.is_file():
        return []
    try:
        payload = json.loads(session.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return []
    reviewers = payload.get("reviewers")
    if isinstance(reviewers, dict):  # token -> name
        return sorted(set(reviewers.values()))
    if isinstance(reviewers, list):
        return sorted({r for r in reviewers if isinstance(r, str)})
    return []


def servable_clips(db_path: Path, table: list[tuple[str, str]]) -> list[tuple[str, int]]:
    """(audio_path, duration_ms) for every clip the queue would hand out, before dialect.

    The WHERE clause is `db::pending_segment_ids_for`'s, and the on-disk check is the one it does
    per distinct path — a row whose audio is gone is not work anybody can do.
    """
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT audio_path, duration_ms FROM speech_segments "
            " WHERE verified = 0 "
            "   AND TRIM(COALESCE(raw_transcript, '')) <> '' "
            "   AND NOT (TRIM(raw_transcript) LIKE '[%]')"
        ).fetchall()
    finally:
        con.close()
    on_disk: dict[str, bool] = {}
    out = []
    for path, duration in rows:
        if path not in on_disk:
            on_disk[path] = os.path.isfile(path)
        if on_disk[path]:
            out.append((path, duration or 0))
    return out


def wrong_dialect_decisions(db_path: Path, roster: dict[str, list[str]], table: list[tuple[str, str]]) -> dict[str, int]:
    """Verified decisions made by a reviewer on a dialect they are not allowed to judge.

    Reported, never failed: the 12 that exist predate the roster, and a gate that stays RED on
    unfixable history buries the signal it was written to carry. It is here because the count should
    only ever go DOWN — a NEW one means the routing broke again, and this is where that shows up.
    """
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT DISTINCT e.reviewer, e.segment_id, s.audio_path FROM review_events e "
            "  JOIN speech_segments s ON s.id = e.segment_id "
            " WHERE e.action IN ('accept','edit','reject') AND s.verified = 1"
        ).fetchall()
    finally:
        con.close()
    offenders: dict[str, int] = {}
    for reviewer, _segment_id, path in rows:
        allowed = roster.get(reviewer)
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
        allowed = roster.get(who)
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

    reviewers = live_reviewers(data_dir)
    if not reviewers:
        print("REVIEWER QUEUES: OK (no couch session — no links are live)", flush=True)
        return 0

    table = source_dialects((_repo_root() / "src-tauri" / "src" / "dialect.rs").read_text(encoding="utf-8"))
    roster = load_roster(data_dir)
    clips = servable_clips(db_path, table)

    problems, warnings = evaluate_queues(reviewers=reviewers, roster=roster, clips=clips, table=table)

    offenders = wrong_dialect_decisions(db_path, roster, table)
    if offenders:
        total = sum(offenders.values())
        detail = ", ".join(f"{who} {n}" for who, n in sorted(offenders.items()))
        warnings.append(
            f"{total} verified decision(s) were made on a dialect the reviewer may not judge ({detail}). "
            f"These predate the roster; they are still verified and carry weight downstream, so they "
            f"need re-reviewing. This count must only ever go DOWN."
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
