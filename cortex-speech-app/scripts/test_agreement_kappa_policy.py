#!/usr/bin/env python3
"""Unit self-check + policy for agreement_kappa.py (dependency-free; always runs).

Anchors Cohen's kappa on the canonical textbook example (kappa = 0.40) so a broken formula can't ship a
flattering agreement number, and pins the degenerate-case conventions + the >2-rater note.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import agreement_kappa as ak  # noqa: E402

SRC = (Path(__file__).resolve().parent / "agreement_kappa.py").read_text(encoding="utf-8")


def test_textbook_kappa_is_0_40() -> None:
    # Two raters, 50 items, confusion [[YY=20, YN=5],[NY=10, NN=15]] -> kappa = 0.40 (Wikipedia example).
    a = ["Y"] * 20 + ["Y"] * 5 + ["N"] * 10 + ["N"] * 15
    b = ["Y"] * 20 + ["N"] * 5 + ["Y"] * 10 + ["N"] * 15
    assert abs(ak.cohen_kappa(a, b) - 0.40) < 1e-9, ak.cohen_kappa(a, b)


def test_perfect_and_chance_edge_cases() -> None:
    assert ak.cohen_kappa(["a", "b", "c"], ["a", "b", "c"]) == 1.0  # total agreement
    assert ak.cohen_kappa(["x", "x", "x"], ["x", "x", "x"]) == 1.0  # single label, all agree -> 1.0
    # Independent halves: po == pe -> kappa == 0 (agreement no better than chance).
    a = ["Y", "Y", "N", "N"]
    b = ["Y", "N", "Y", "N"]
    assert abs(ak.cohen_kappa(a, b)) < 1e-9, ak.cohen_kappa(a, b)


def test_length_and_empty_guards() -> None:
    try:
        ak.cohen_kappa(["a"], ["a", "b"])
    except ValueError:
        pass
    else:
        raise AssertionError("unequal-length rater sequences must raise")
    try:
        ak.cohen_kappa([], [])
    except ValueError:
        pass
    else:
        raise AssertionError("empty input must raise")


def test_interpretation_bands() -> None:
    assert ak.interpret(0.40) == "fair"  # 0.21-0.40 is "fair"; "moderate" starts at 0.41 (Landis-Koch)
    assert ak.interpret(0.50) == "moderate"
    assert ak.interpret(0.85) == "almost perfect"
    assert ak.interpret(-0.1) == "poor / worse than chance"


def test_source_documents_two_rater_scope() -> None:
    assert "Krippendorff" in SRC, "must point >2-rater users to Krippendorff's alpha"
    assert "label ceiling" in SRC.lower() or "caps" in SRC.lower(), "must state the gold-ceiling purpose"


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
        print(f"\n{failures} agreement-kappa test(s) failed", flush=True)
        return 1
    print("\nagreement-kappa tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
