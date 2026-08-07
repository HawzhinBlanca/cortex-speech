#!/usr/bin/env python3
"""Every whole-library `get_segments` read must be a DELIBERATE, justified exception.

External review 2026-08-06 #6: "Several commands still load the complete segment library. Move
filtering and aggregation into SQL and require pagination/cursors."

This gate is the REQUIRE half. It does not make the existing reads bounded — it makes the set of them
closed, so the population can only shrink. A new `db.get_segments(None)` in a command is now a build
failure with a message pointing at the paginated API, instead of a quiet O(corpus) read that nobody
notices until the library is large.

Being explicit about what this does NOT do, because a gate that oversells itself is worse than none:
the entries below are still unbounded. Retiring them is real work with different shapes per site —
`get_active_learning_queue` needs its conformal threshold computed in SQL (it reads VERIFIED rows to
calibrate and UNVERIFIED rows as candidates, which together is the whole corpus, so no cursor helps),
while the export and quality-metric paths are whole-corpus BY DESIGN and are expected to stay.

Two categories, and the distinction is the point:
  - BY DESIGN: the operation is defined over the entire corpus. An export that skipped rows would be
    wrong, not slow. These are permanent.
  - TO RETIRE: bounded in principle, unbounded today. These are the actual #6 backlog.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SRC = REPO_ROOT / "src-tauri" / "src"

# (relative path, justification). A site is keyed by FILE, not line, so ordinary edits above it do not
# churn this list; the count per file is what is pinned.
BY_DESIGN = {
    "export.rs": "an export is defined over the whole corpus — skipping rows would be wrong, not slow",
    "export_bundle.rs": "same: the bundle must describe every row its data files contain",
    "eval.rs": "the fine-tune pack ships every training-ready row by definition",
    "quality.rs": "corpus-wide quality metrics (duplicate groups, duration outliers) need every row",
    "jury/mod.rs": "adjudication reconciles the whole verified set",
    "integration_runner.rs": "an integration run asserts over the complete library",
    "validation/mod.rs": "a validation report is a statement about every row in the corpus",
}

TO_RETIRE = {
    # Three whole-corpus STATISTICS (training-grade breakdown, conformal certificate, annotation-drift
    # scorecard). Naively "aggregate it in SQL" is the wrong retirement here and would trade this bug for
    # a worse one: each statistic is computed by a shared Rust policy function — the same
    # `training_grade_for_segment` the EXPORT gates on — so a SQL reimplementation would fork the policy
    # and let the dashboard and the export disagree about what is training-ready. The correct retirement
    # is a STREAMING read (fold row-by-row into the counters/score vector instead of collecting a Vec of
    # full rows), which bounds memory while leaving exactly one implementation of the rule.
    "commands/dataset_analytics.rs": "fold over a streaming read; do NOT reimplement the grading policy in SQL",
    "commands/segments_read.rs": "get_active_learning_queue needs its conformal threshold computed in SQL",
}

# Retired, kept as a record so the list cannot quietly regrow into them:
#   couch.rs (2026-08-07) — api_queue read every pending row's FULL record (transcript, alignment JSON,
#   evidence JSON) to hand out at most QUEUE_BATCH=25 of them. Its heldByOthers / skippedByYou /
#   pendingTotal counts genuinely need every pending row — they depend on IN-MEMORY lease state, which
#   no SQL aggregate can see — but a row that is only being COUNTED needs nothing except its id. It now
#   walks `Database::pending_segment_ids` and hydrates only the served batch via `get_segments_by_ids`,
#   outside the state lock. `the_queue_serves_clips_in_the_order_it_leased_them` pins the re-index that
#   keeps the served order equal to the leased order, since get_segments_by_ids imposes its own.
#
#   commands.rs (2026-08-06) — the 7B refinement driver, the CTC-score backfill and the signal-anomaly
#   backfill each read the whole library and then `continue`d past every row that was already done. All
#   three now ask for their backlog via `Database::get_pending_segments(PendingWork::_)`. The 7B
#   predicate keeps `segment_awaits_wsl7b` as its authority and uses SQL only as a deliberate superset;
#   `db_tests::sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate` pins that they cannot drift.

ALLOWED = {**BY_DESIGN, **TO_RETIRE}

CALL = re.compile(r"\bdb\.get_segments\(\s*(None|Some\()")


def _production_files() -> list[Path]:
    out = []
    for path in sorted(SRC.rglob("*.rs")):
        name = path.name
        if name.endswith("_tests.rs") or name == "db.rs" or "/bin/" in path.as_posix():
            continue
        out.append(path)
    return out


def test_no_new_unbounded_segment_read_appears() -> None:
    found: dict[str, int] = {}
    for path in _production_files():
        rel = path.relative_to(SRC).as_posix()
        # Strip the test module so a fixture's whole-library read is not counted as production.
        body = path.read_text(encoding="utf-8", errors="ignore").split("mod tests")[0]
        hits = len(CALL.findall(body))
        if hits:
            found[rel] = hits

    unexpected = sorted(set(found) - set(ALLOWED))
    if unexpected:
        raise AssertionError(
            "new whole-library get_segments read(s) in "
            + ", ".join(unexpected)
            + " — a command must page (get_segments_page / a cursor) or aggregate in SQL. If the "
            "operation is genuinely corpus-wide, add it to BY_DESIGN here with the reason."
        )

    # A file that no longer reads the whole library must be REMOVED from the list, or the gate keeps
    # blessing a path that does not exist and quietly loses its teeth.
    stale = sorted(set(ALLOWED) - set(found))
    if stale:
        raise AssertionError(
            "these files no longer read the whole library: "
            + ", ".join(stale)
            + " — delete them from ALLOWED so the exception list stays honest (and move the entry to "
            "the ledger as retired if it was in TO_RETIRE)"
        )


def test_the_backlog_is_named_rather_than_implied() -> None:
    """TO_RETIRE must not silently empty out into 'everything is by design'."""
    if not TO_RETIRE:
        raise AssertionError(
            "TO_RETIRE is empty. If every unbounded read really is by design, say so in the ledger "
            "and delete this assertion deliberately — do not let the backlog vanish by attrition."
        )
    overlap = set(BY_DESIGN) & set(TO_RETIRE)
    if overlap:
        raise AssertionError(f"a site cannot be both by-design and to-retire: {sorted(overlap)}")


def main() -> None:
    test_no_new_unbounded_segment_read_appears()
    test_the_backlog_is_named_rather_than_implied()
    print(f"unbounded segment-read policy passed ({len(BY_DESIGN)} by design, {len(TO_RETIRE)} to retire)")


if __name__ == "__main__":
    main()
