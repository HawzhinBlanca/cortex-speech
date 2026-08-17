#!/usr/bin/env python3
"""Every arm of the promotion gate, because its whole job is to REFUSE.

A gate that says PROMOTE when it should not is worse than no gate: it launders a worse model into
the champion slot with a document that says it was proven. So the tests below spend most of their
effort on the refusals, and the one PROMOTE case has to earn it.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from promotion_gate import decide  # noqa: E402

BASIS = "nfc_lower_ws"


def rows(per_clip, ref=100):
    """{clip: char_dist} -> the scorecard shape, with a `ref`-char reference per clip."""
    return {clip: (dist, ref, dist, max(1, ref // 5)) for clip, dist in per_clip.items()}


def test_a_clear_significant_improvement_promotes() -> None:
    champion = rows({i: 20 for i in range(40)})
    # Better on every clip, by a lot — the only case that should ever pass.
    challenger = rows({i: 10 for i in range(40)})
    report = decide(champion, challenger, BASIS, BASIS, {})
    assert report["verdict"] == "PROMOTE", report["reasons"]
    assert report["champion_cer"] == 0.20 and report["challenger_cer"] == 0.10


def test_no_improvement_is_rejected() -> None:
    champion = rows({i: 20 for i in range(40)})
    report = decide(champion, dict(champion), BASIS, BASIS, {})
    assert report["verdict"] == "REJECT"
    assert any("did not improve" in r for r in report["reasons"]), report["reasons"]


def test_a_worse_challenger_is_rejected() -> None:
    champion = rows({i: 20 for i in range(40)})
    challenger = rows({i: 30 for i in range(40)})
    report = decide(champion, challenger, BASIS, BASIS, {})
    assert report["verdict"] == "REJECT"


def test_an_improvement_inside_the_noise_is_rejected() -> None:
    """0.05 CER points is not progress; it is a different afternoon."""
    # A long reference, so one character of improvement is genuinely tiny: 1/10000 = 0.0001 CER,
    # below the 0.001 floor. (The first version of this fixture used a 100-char reference, where one
    # character is 0.01 CER — ten times ABOVE the floor — so it promoted, correctly, and caught the
    # fixture rather than the gate.)
    champion = rows({i: 2000 for i in range(40)}, ref=10000)
    challenger = rows({i: 1999 for i in range(40)}, ref=10000)
    report = decide(champion, challenger, BASIS, BASIS, {})
    assert report["verdict"] == "REJECT"
    assert any("inside the noise" in r or "improved by only" in r for r in report["reasons"]), report["reasons"]


def test_an_insignificant_improvement_is_rejected() -> None:
    """Better on average, but the per-clip spread says it could be chance."""
    champion = rows({i: 20 for i in range(40)})
    # Half the clips much better, half much worse: mean improves slightly, variance is huge.
    challenger = rows({i: (2 if i % 2 == 0 else 35) for i in range(40)})
    report = decide(champion, challenger, BASIS, BASIS, {})
    assert report["verdict"] == "REJECT"
    assert any("not significant" in r for r in report["reasons"]), report["reasons"]


def test_a_regressed_protected_slice_blocks_an_overall_win() -> None:
    """The audit's point: an average dominated by one voice can hide harm to everyone else."""
    champion = rows({i: 20 for i in range(40)})
    challenger = rows({i: (5 if i < 35 else 60) for i in range(40)})  # big overall win, slice 35-39 ruined
    slices = {"badini": list(range(35, 40)), "hawleri": list(range(0, 20))}
    report = decide(champion, challenger, BASIS, BASIS, slices)
    assert report["verdict"] == "REJECT"
    assert any("REGRESSED" in r and "badini" in r for r in report["reasons"]), report["reasons"]
    # And it still reports the numbers for every slice, including the healthy one.
    assert {e["slice"] for e in report["slices"]} == {"badini", "hawleri"}


def test_an_unscoreable_slice_is_refused_not_ignored() -> None:
    """A protected group with no paired clips cannot be confirmed safe, so it is not waved through."""
    champion = rows({i: 20 for i in range(40)})
    challenger = rows({i: 10 for i in range(40)})
    report = decide(champion, challenger, BASIS, BASIS, {"badini": [900, 901]})
    assert report["verdict"] == "REJECT"
    assert any("could not be scored" in r for r in report["reasons"]), report["reasons"]


def test_an_unpaired_comparison_is_invalid() -> None:
    """Different clip sets measure the test set as much as the model."""
    champion = rows({i: 20 for i in range(40)})
    challenger = rows({i: 10 for i in range(30)})  # skipped 10 clips
    report = decide(champion, challenger, BASIS, BASIS, {})
    assert report["verdict"] == "INVALID"
    assert any("unpaired" in r for r in report["reasons"]), report["reasons"]


def test_a_cross_basis_comparison_is_invalid() -> None:
    """Two CERs normalised differently are not on one scale."""
    champion = rows({i: 20 for i in range(40)})
    challenger = rows({i: 10 for i in range(40)})
    report = decide(champion, challenger, BASIS, "raw", {})
    assert report["verdict"] == "INVALID"
    assert any("normalization basis differs" in r for r in report["reasons"]), report["reasons"]


def test_nothing_in_common_is_invalid() -> None:
    report = decide(rows({1: 5}), rows({2: 5}), BASIS, BASIS, {})
    assert report["verdict"] == "INVALID"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PROMOTION GATE: {len(tests)} arms pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
