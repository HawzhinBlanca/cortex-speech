#!/usr/bin/env python3
"""Deterministic canonical-selection pins for the review-pool dedup manifest (schema 2, 2026-09-06).

The Rust validator (`dedup.rs::apply_superseding_manifest`) re-derives the same choice from the frozen
pool, so these pins protect the ONE rule both sides must agree on:
applied canonical -> most human review evidence -> best measured audio quality -> stable identity.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_review_pool_dedup_manifest import canonical_member, scaled_metric  # noqa: E402


def member(segment_id: str, *, reviews: int = 0, snr: float | None = None, clipping: float | None = None):
    return {
        "segmentId": segment_id,
        "sourceFileName": f"{segment_id}.wav",
        "reviewEvidenceCount": reviews,
        "snrMilliDb": None if snr is None else round(snr * 1_000),
        "clippingPpm": None if clipping is None else round(clipping * 1_000_000),
        "signalAnomalyPpm": None,
        "confidencePpm": None,
    }


def test_reviewed_member_always_wins_over_unreviewed_quality() -> None:
    selected, reason = canonical_member(
        [member("reviewed", reviews=1, snr=5.0), member("cleaner", snr=40.0)], set()
    )
    assert selected["segmentId"] == "reviewed"
    assert reason == "preserve-most-human-review-evidence"


def test_most_human_evidence_wins_and_an_exact_tie_falls_back_to_quality() -> None:
    """v1 refused a family with two reviewed twins; v2 keeps the twin with MORE evidence (the other
    keeps its evidence and leaves serving), and breaks an exact tie on the stable quality key."""
    selected, reason = canonical_member([member("once", reviews=1, snr=40.0), member("twice", reviews=2, snr=5.0)], set())
    assert (selected["segmentId"], reason) == ("twice", "preserve-most-human-review-evidence")
    selected, reason = canonical_member([member("a", reviews=1, snr=5.0), member("b", reviews=1, snr=40.0)], set())
    assert (selected["segmentId"], reason) == ("b", "preserve-most-human-review-evidence")


def test_an_applied_canonical_stays_even_against_a_reviewed_newcomer() -> None:
    """Its v1 exclusion rows are immutable and point at it; a newcomer's review cannot move them."""
    selected, reason = canonical_member(
        [member("applied", snr=5.0), member("newcomer", reviews=2, snr=40.0)], {"applied"}
    )
    assert (selected["segmentId"], reason) == ("applied", "preserve-applied-canonical")


def test_two_applied_canonicals_merge_by_evidence_then_quality() -> None:
    """Two applied v1 families proven to be one recording: the applied canonical with more evidence
    stays, the other retires under it (its own exclusions chain one hop to the live root)."""
    selected, reason = canonical_member(
        [member("first", snr=40.0), member("second", reviews=1, snr=5.0), member("twin")],
        {"first", "second"},
    )
    assert (selected["segmentId"], reason) == ("second", "preserve-applied-canonical")
    selected, _ = canonical_member([member("first", snr=5.0), member("second", snr=40.0)], {"first", "second"})
    assert selected["segmentId"] == "second", "an exact evidence tie between applied canonicals uses the quality key"


def test_quality_then_stable_identity_selects_deterministically() -> None:
    selected, reason = canonical_member(
        [member("b", snr=20.0, clipping=0.01), member("a", snr=20.0, clipping=0.0)], set()
    )
    assert selected["segmentId"] == "a"
    assert reason == "best-measured-audio-quality-then-stable-identity"


def test_scaled_metric_matches_rust_half_away_from_zero() -> None:
    assert scaled_metric(0.0000005, 1_000_000) == 1
    assert scaled_metric(-0.0000005, 1_000_000) == -1
    assert scaled_metric(None, 1_000_000) is None


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"REVIEW POOL DEDUP MANIFEST: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
