#!/usr/bin/env python3
"""CI-safe unit tests for asr_metrics.py (P2.1) — WER math, micro rate, bootstrap, RTF."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from asr_metrics import bootstrap_ci, edit_distance, micro, rtf, word_pair  # noqa: E402


def test_edit_distance_basic() -> None:
    assert edit_distance(list("kitten"), list("sitting")) == 3
    assert edit_distance([], list("abc")) == 3
    assert edit_distance(list("same"), list("same")) == 0


def test_word_pair_counts_token_edits_not_chars() -> None:
    # One word substituted out of three -> distance 1, ref length 3.
    assert word_pair("ئەو ساڵە باش", "ئەو ساڵە خراپ") == (1, 3)
    # Identical -> zero distance.
    assert word_pair("hello world", "hello world") == (0, 2)
    # Empty reference -> zero ref length (drops out of the ratio upstream).
    assert word_pair("", "anything here") == (2, 0)


def test_micro_is_ratio_of_sums() -> None:
    # Σdist=1+2=3, Σref=3+2=5 -> 0.6, NOT the mean of per-clip rates.
    assert abs(micro([(1, 3), (2, 2)]) - 0.6) < 1e-12
    assert micro([]) == 0.0


def test_bootstrap_ci_is_seeded_and_ordered() -> None:
    per_clip = [(1, 10), (0, 8), (2, 12), (1, 9)]
    lo1, hi1 = bootstrap_ci(per_clip, n_boot=500, seed=42)
    lo2, hi2 = bootstrap_ci(per_clip, n_boot=500, seed=42)
    assert (lo1, hi1) == (lo2, hi2), "same seed -> identical CI (deterministic)"
    assert lo1 <= hi1, "CI bounds ordered"
    assert bootstrap_ci([], n_boot=100) == (0.0, 0.0)


def test_rtf_handles_unknown_duration() -> None:
    assert rtf(2.0, 8.0) == 0.25
    assert rtf(5.0, 0.0) is None, "unknown audio duration -> None, never a divide-by-zero"


def _run() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            fn()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {name}: {exc}", flush=True)
        else:
            print(f"ok   {name}", flush=True)
    if failures:
        print(f"\n{failures} asr-metrics test(s) failed", flush=True)
        return 1
    print("\nasr-metrics tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
