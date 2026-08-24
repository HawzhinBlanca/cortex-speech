#!/usr/bin/env python3
"""Deterministic canonical-selection pins for the review-pool dedup manifest."""

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
        [member("reviewed", reviews=1, snr=5.0), member("cleaner", snr=40.0)]
    )
    assert selected["segmentId"] == "reviewed"
    assert reason == "preserve-human-review-evidence"


def test_multiple_reviewed_duplicates_fail_closed() -> None:
    try:
        canonical_member([member("a", reviews=1), member("b", reviews=1)])
    except ValueError as error:
        assert "multiple members" in str(error)
    else:
        raise AssertionError("ambiguous reviewed duplicate family must be refused")


def test_quality_then_stable_identity_selects_deterministically() -> None:
    selected, reason = canonical_member(
        [member("b", snr=20.0, clipping=0.01), member("a", snr=20.0, clipping=0.0)]
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
