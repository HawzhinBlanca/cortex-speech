#!/usr/bin/env python3
"""Unit self-check + policy for calibration_ece.py (dependency-free; always runs).

Anchors ECE on known-answer examples so a broken formula can't ship a flattering (too-low) calibration
number, and pins the honesty note that the offline 0.90 heuristic makes ECE meaningless.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import calibration_ece as ce  # noqa: E402

SRC = (Path(__file__).resolve().parent / "calibration_ece.py").read_text(encoding="utf-8")


def test_perfect_calibration_is_zero() -> None:
    # conf 1.0 and all correct -> stated == observed -> ECE 0.
    ece, _ = ce.expected_calibration_error([1.0] * 10, [1] * 10, n_bins=10)
    assert abs(ece) < 1e-9, ece
    # conf 0.5 with exactly 50% correct in that bin -> also 0.
    ece2, _ = ce.expected_calibration_error([0.55] * 10, [1] * 5 + [0] * 5, n_bins=10)
    # bin [0.5,0.6): mean_conf 0.55, accuracy 0.5 -> gap 0.05.
    assert abs(ece2 - 0.05) < 1e-9, ece2


def test_known_miscalibration() -> None:
    # 10 predictions all at confidence 0.8, but only 5 correct -> one bin: conf 0.8, acc 0.5 -> ECE 0.3.
    ece, bins = ce.expected_calibration_error([0.8] * 10, [1] * 5 + [0] * 5, n_bins=10)
    assert abs(ece - 0.3) < 1e-9, ece
    assert len(bins) == 1 and bins[0]["count"] == 10, bins


def test_weighted_across_bins() -> None:
    # bin A: 8 preds @ conf 0.9, 6 correct (acc 0.75, gap 0.15); bin B: 2 preds @ conf 0.1, 0 correct
    # (acc 0.0, gap 0.1). ECE = 0.8*0.15 + 0.2*0.1 = 0.14.
    conf = [0.9] * 8 + [0.1] * 2
    corr = [1] * 6 + [0] * 2 + [0] * 2
    ece, _ = ce.expected_calibration_error(conf, corr, n_bins=10)
    assert abs(ece - 0.14) < 1e-9, ece


def test_confidence_one_lands_in_last_bin() -> None:
    # 1.0 must be counted (right-edge of the last bin), not dropped.
    ece, bins = ce.expected_calibration_error([1.0, 1.0], [1, 0], n_bins=10)
    assert sum(b["count"] for b in bins) == 2, bins
    assert abs(ece - 0.5) < 1e-9, ece  # conf 1.0, acc 0.5 -> gap 0.5


def test_guards() -> None:
    bad_cases = (
        lambda: ce.expected_calibration_error([0.5], [1, 0]),  # length mismatch
        lambda: ce.expected_calibration_error([], []),  # empty
        # A NaN / >1 / <0 confidence must be REFUSED, not silently dropped from every bin while still
        # counting toward n (which scales ECE down — a fabricated, better-looking calibration number).
        lambda: ce.expected_calibration_error([float("nan"), 0.5], [1, 0]),
        lambda: ce.expected_calibration_error([1.5, 0.5], [1, 0]),
        lambda: ce.expected_calibration_error([-0.1, 0.5], [1, 0]),
        lambda: ce.expected_calibration_error([float("inf"), 0.5], [1, 0]),
    )
    for bad in bad_cases:
        try:
            bad()
        except ValueError:
            pass
        else:
            raise AssertionError("expected ValueError on bad input")


def test_source_documents_heuristic_caveat() -> None:
    assert "0.90" in SRC and "heuristic" in SRC.lower(), "must warn that the offline 0.90 heuristic voids ECE"
    assert "real" in SRC.lower() and "posterior" in SRC.lower(), "must say to run only over real posteriors"


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
        print(f"\n{failures} calibration-ece test(s) failed", flush=True)
        return 1
    print("\ncalibration-ece tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
