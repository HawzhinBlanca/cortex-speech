"""OWNER CANON 2026-08-29: a sentence is decided by any two DIFFERENT reviewers.

WHY THIS EXISTS: the canon before this one was also implemented, and it still quietly became the
blocker. The sequential campaign counted only the single reviewer named in its policy, so 27
decisions by other people on focus clips counted for nothing and the corpus could not ship. A review
model is not safe merely because it is written down; the parts that make it true have to be pinned
where a machine can see them drift.

Each pin below is a load-bearing line, not a coincidence of wording. If one disappears, either the
canon changed (which requires the owner writing `change canon:`) or something silently un-implemented
it.

Run: python scripts/test_consensus_review_canon.py
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "cortex-speech-app"
POOL = APP / "src-tauri" / "src" / "review_pool.rs"
EXPORT = APP / "src-tauri" / "src" / "export.rs"
CANON = ROOT / "docs" / "OWNER_CANON.md"

FAILURES: list[str] = []


def require(path: Path, needle: str, why: str) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        FAILURES.append(f"{path.name}: unreadable ({error})")
        return
    # Prose wraps. A pin on a sentence in a markdown document must not depend on where the line
    # happened to break, so whitespace is collapsed before comparing for those.
    if path.suffix.lower() == ".md":
        text = " ".join(text.split())
        needle = " ".join(needle.split())
    if needle not in text:
        FAILURES.append(f"{path.name}: MISSING {needle!r}\n      because: {why}")
    else:
        print(f"  ok  {path.name}: {why}")


def test_two_different_reviewers_decide_a_sentence() -> None:
    require(
        POOL,
        "(names.len() >= 2).then_some((outcome, names))",
        "an outcome resolves only when two or more DISTINCT reviewers agree on it",
    )
    require(
        POOL,
        "2 => DerivedResolution::NeedsThird",
        "two reviewers who disagree escalate to a third, they do not decide",
    )
    require(
        POOL,
        "0 | 1 => DerivedResolution::Pending",
        "one opinion is never a decision",
    )


def test_only_decided_sentences_may_be_exported() -> None:
    require(
        POOL,
        "pub fn consensus_resolved_segment_ids",
        "the export asks the pool which sentences are DECIDED",
    )
    require(
        POOL,
        'matches!(row.status.as_str(), "resolved" | "ownerResolved")',
        "only resolved or owner-adjudicated clips count as decided",
    )
    require(
        EXPORT,
        "crate::review_pool::consensus_resolved_segment_ids(db)",
        "the shared export root consults consensus, so no export path can skip it",
    )
    require(
        EXPORT,
        "export: dropping segment no two reviewers have decided",
        "an undecided clip is dropped from every export and the drop is logged",
    )
    require(
        EXPORT,
        "nothing is exportable yet:",
        "when consensus is the only reason a pack is empty the export REFUSES and names the count, "
        "because a silently empty pack reads as a broken button rather than a rule doing its job",
    )


def test_any_reviewer_may_take_any_clip() -> None:
    """The pool is flexible by construction: no clip is assigned to a named person.

    The ONLY restriction is per clip -- the same person may not be two of its opinions. That is what
    lets a new reviewer be added and start working immediately, and what lets throughput scale with
    however many people are online.
    """
    require(
        POOL,
        "if coverage.is_some_and(|coverage| coverage.seen.contains(&reviewer)) {",
        "a reviewer is never served a clip they already judged - independence is enforced PER CLIP",
    )
    require(
        POOL,
        "pub fn pending_segment_ids(",
        "one queue function serves every reviewer; there is no per-reviewer assignment table",
    )


def test_the_queue_serves_what_is_nearest_a_decision() -> None:
    require(
        POOL,
        "let distance_to_decision: usize = match judged {",
        "the queue orders by how close a clip is to being decided, not by how untouched it is",
    )
    require(
        POOL,
        "pending.push((distance_to_decision, created_at, segment_id));",
        "that ordering is what the queue actually pushes",
    )


def test_the_canon_is_written_down() -> None:
    require(
        CANON,
        "any two DIFFERENT reviewers",
        "the canon itself is recorded for humans, not only enforced in code",
    )
    require(
        CANON,
        "enforced PER CLIP",
        "independence is per clip, never per person - the rule that lets throughput scale",
    )


def test_the_pins_would_actually_notice() -> None:
    """Anti-vacuity: a pin that cannot fail proves nothing."""
    before = len(FAILURES)
    require(POOL, "a_string_that_must_never_appear_in_review_pool_rs", "probe")
    if len(FAILURES) != before + 1:
        raise AssertionError("the pin helper did not report a missing needle - these pins are vacuous")
    FAILURES.pop()
    print("  ok  the pin helper reports a missing needle (pins are not vacuous)")


if __name__ == "__main__":
    test_the_pins_would_actually_notice()
    test_two_different_reviewers_decide_a_sentence()
    test_any_reviewer_may_take_any_clip()
    test_only_decided_sentences_may_be_exported()
    test_the_queue_serves_what_is_nearest_a_decision()
    test_the_canon_is_written_down()
    if FAILURES:
        print("\nCONSENSUS REVIEW CANON: FAIL", file=sys.stderr)
        for failure in FAILURES:
            print(f"  - {failure}", file=sys.stderr)
        raise SystemExit(1)
    print("CONSENSUS REVIEW CANON: all pins hold")
