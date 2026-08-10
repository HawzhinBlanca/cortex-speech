#!/usr/bin/env python3
"""Extract double-annotated items from the live app DB into the TSV `agreement_kappa.py` reads.

WHY THIS EXISTS. `iaa-kappa-ceiling` (charter item 44) is the gate that bounds every accuracy number
this project may honestly quote: a gold set built at kappa~=0.80 caps measurable model accuracy at
~0.80. It has been owner-gated on "recruit >=2 independent Sorani annotators" — but recruiting them
was never the whole blocker. `agreement_kappa.py` reads a TSV of two rater columns, and NOTHING in the
repo produced one. Even with two annotators working, computing kappa meant hand-assembling a file out
of SQLite, which is exactly how a mandatory gate stays permanently manual.

The data was already there. Couch Review serves SPOT CHECKS unleased on purpose — two reviewers meeting
the same known-answer clip is the point, not a collision — so `spot_checks` already holds one row per
(segment, reviewer). Two reviewers on one segment IS the double annotation.

WHAT IT REFUSES TO DO. It will not write a file that produces a meaningless number:

  * zero overlapping items -> exit 2, no file. `cohen_kappa` on an empty set raises, but a
    one-or-two-item file would "succeed" and print a kappa built from nothing.
  * fewer than --min-items overlaps -> exit 2 by default. Kappa on a handful of items has a
    confidence interval so wide it cannot support the claim the gate exists to make.
  * more than two reviewers -> the two with the most SHARED items are used and every excluded
    reviewer is NAMED. Cohen's kappa is the 2-rater statistic; silently pooling three raters into
    two columns invents agreement that nobody measured (use Krippendorff's alpha for >2).

Usage:
    python scripts/build_iaa_annotations.py [--db <path>] [--out <path>] [--min-items N]
    python scripts/agreement_kappa.py <out>
"""

from __future__ import annotations

import argparse
import collections
import os
import sqlite3
import sys
from pathlib import Path


def default_db() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech" / "cortex-speech.db"
    return Path.home() / ".local" / "share" / "cortex-speech" / "cortex-speech.db"


def load_annotations(db_path: Path) -> dict[str, dict[str, str]]:
    """{segment_id: {reviewer: label}} from spot_checks — the only table with a KNOWN answer per item.

    Read-only (mode=ro): this runs against the owner's live DB while the app may be writing.
    """
    if not db_path.is_file():
        raise SystemExit(f"database not found: {db_path}")
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT segment_id, reviewer, action FROM spot_checks "
            "WHERE reviewer IS NOT NULL AND action IS NOT NULL AND TRIM(action) <> ''"
        ).fetchall()
    except sqlite3.Error as exc:
        raise SystemExit(f"could not read spot_checks: {exc}") from exc
    finally:
        con.close()

    by_segment: dict[str, dict[str, str]] = collections.defaultdict(dict)
    for segment_id, reviewer, action in rows:
        # Last write wins for a reviewer who revisited an item; the pair is (segment, reviewer).
        by_segment[segment_id][reviewer.strip()] = action.strip()
    return by_segment


def choose_pair(by_segment: dict[str, dict[str, str]]) -> tuple[str, str, list[str]]:
    """The two reviewers sharing the most items, plus everyone excluded (named, never silent)."""
    shared: collections.Counter = collections.Counter()
    reviewers: set[str] = set()
    for labels in by_segment.values():
        names = sorted(labels)
        reviewers.update(names)
        for i, first in enumerate(names):
            for second in names[i + 1:]:
                shared[(first, second)] += 1
    if not shared:
        # Exit 2, same as every other refusal here: "I will not produce a misleading number" must be
        # one distinguishable status, not sometimes 1 and sometimes 2.
        print(
            "no segment has been annotated by two different reviewers yet — nothing to measure.\n"
            f"reviewers seen: {sorted(reviewers) or 'none'}\n"
            "Couch Review serves spot checks to EVERY reviewer, so this fills in as they work.",
            file=sys.stderr,
        )
        raise SystemExit(2)
    (rater_a, rater_b), _ = shared.most_common(1)[0]
    excluded = sorted(reviewers - {rater_a, rater_b})
    return rater_a, rater_b, excluded


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", type=Path, default=default_db())
    parser.add_argument("--out", type=Path, default=Path("iaa_annotations.tsv"))
    parser.add_argument(
        "--min-items", type=int, default=30,
        help="refuse to emit fewer than this many doubly-annotated items (default 30)",
    )
    args = parser.parse_args()

    by_segment = load_annotations(args.db)
    rater_a, rater_b, excluded = choose_pair(by_segment)

    pairs = [
        (segment_id, labels[rater_a], labels[rater_b])
        for segment_id, labels in sorted(by_segment.items())
        if rater_a in labels and rater_b in labels
    ]

    print(f"raters: {rater_a!r} vs {rater_b!r}")
    if excluded:
        print(f"EXCLUDED (Cohen's kappa is the 2-rater statistic; use Krippendorff's alpha for >2): {excluded}")
    print(f"doubly-annotated items: {len(pairs)}")

    # Independent of --min-items, which is a tunable floor: ZERO doubly-annotated items is not a small
    # sample, it is no measurement at all, and `--min-items 0` must not be able to turn that into an
    # empty TSV that agreement_kappa would then be asked to score.
    if not pairs:
        print(
            f"no item was annotated by both {rater_a!r} and {rater_b!r} — nothing to measure.",
            file=sys.stderr,
        )
        return 2

    if len(pairs) < args.min_items:
        print(
            f"\nREFUSING to write {args.out}: {len(pairs)} item(s) is below --min-items {args.min_items}.\n"
            "A kappa from a handful of items has a confidence interval too wide to support the claim\n"
            "this gate exists to make. Lower the floor deliberately with --min-items if you truly want it.",
            file=sys.stderr,
        )
        return 2

    lines = [f"{rater_a}\t{rater_b}\tsegment_id"]
    lines += [f"{a}\t{b}\t{sid}" for sid, a, b in pairs]
    args.out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"\nwrote {args.out} ({len(pairs)} items)")
    print(f"next: python scripts/agreement_kappa.py {args.out}")

    counts_a = collections.Counter(a for _, a, _ in pairs)
    counts_b = collections.Counter(b for _, _, b in pairs)
    print(f"  {rater_a} label mix: {dict(counts_a)}")
    print(f"  {rater_b} label mix: {dict(counts_b)}")
    agree = sum(1 for _, a, b in pairs if a == b)
    print(f"  raw agreement: {agree}/{len(pairs)} = {agree / len(pairs):.1%} (kappa corrects this for chance)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
