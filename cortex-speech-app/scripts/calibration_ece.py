#!/usr/bin/env python3
"""Confidence calibration: Expected Calibration Error (ECE) + a reliability table.

Pairs with scripts/agreement_kappa.py: kappa measures the GOLD SET's label ceiling; ECE measures whether
the model's CONFIDENCE can be trusted. Before the conformal/jury/autonomy stack (which calibrates on
seg.confidence) can be believed, the confidences must be shown to MEAN something — a model that says "0.95"
should be right ~95% of the time. ECE is the gap between stated confidence and observed accuracy, averaged
over confidence bins (docs/ASR_TECH_SCAN_2026-07-23.md §4).

IMPORTANT (honesty): on the offline OmniASR-CTC path, confidence is the fixed 0.90 HEURISTIC (sherpa
exposes no token posteriors — asr.rs ConfidenceSource::Heuristic), so its ECE is meaningless. Run this only
over segments whose confidence is a REAL posterior (the WSL-7B path, confidence_source='real_posterior'),
or the number certifies nothing. This harness computes the metric; it does not decide the source.

Pure stdlib: the ECE math is a dependency-free function unit-tested against known-answer examples, so it
always runs on the policy runner.

Usage:  python scripts/calibration_ece.py <predictions.tsv> [n_bins]
        # TSV: header row, then two columns per prediction: <confidence 0..1>\t<correct 1|0>
"""

from __future__ import annotations

import math
import sys
from pathlib import Path


def expected_calibration_error(confidences, correct, n_bins: int = 10):
    """ECE = sum_b (|B_b|/N) * |acc(B_b) - conf(B_b)|, binning by confidence into n_bins equal-width bins.

    Returns (ece, bins) where bins is a list of dicts (lo, hi, count, mean_conf, accuracy, gap) for the
    NON-EMPTY bins — a reliability table. `correct` is a sequence of 0/1 (or bool).
    """
    if len(confidences) != len(correct):
        raise ValueError(f"confidences and correct must be equal length ({len(confidences)} vs {len(correct)})")
    n = len(confidences)
    if n == 0:
        raise ValueError("no predictions to score")
    if n_bins < 1:
        raise ValueError("n_bins must be >= 1")
    # A confidence must be a FINITE probability in [0, 1]. A NaN / >1 / <0 value would fall in NO bin (the
    # membership filter below) yet still count toward n, silently scaling ECE DOWN — a better-looking,
    # FABRICATED calibration number the honesty law forbids. Refuse it loudly: an out-of-range confidence is
    # an upstream scorer bug, not something to quietly average away. (Mirrors iter-123: a metric that reaches
    # the ledger must never be silently wrong.)
    for i, c in enumerate(confidences):
        if not (isinstance(c, (int, float)) and math.isfinite(c) and 0.0 <= c <= 1.0):
            raise ValueError(f"confidence[{i}] = {c!r} is not a finite probability in [0, 1]; refusing to score")
    ece = 0.0
    bins = []
    for b in range(n_bins):
        lo = b / n_bins
        hi = (b + 1) / n_bins
        # [lo, hi); the last bin also includes the right edge so confidence == 1.0 is counted.
        members = [
            i
            for i in range(n)
            if confidences[i] >= lo and (confidences[i] < hi or (b == n_bins - 1 and confidences[i] <= hi))
        ]
        if not members:
            continue
        mean_conf = sum(confidences[i] for i in members) / len(members)
        accuracy = sum(1 for i in members if correct[i]) / len(members)
        gap = abs(accuracy - mean_conf)
        ece += (len(members) / n) * gap
        bins.append(
            {"lo": lo, "hi": hi, "count": len(members), "mean_conf": mean_conf, "accuracy": accuracy, "gap": gap}
        )
    return ece, bins


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python scripts/calibration_ece.py <predictions.tsv> [n_bins]  (header + <confidence>\\t<correct 1|0>)")
        return 2
    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"predictions file not found: {path}", file=sys.stderr)
        return 2
    n_bins = int(sys.argv[2]) if len(sys.argv) > 2 else 10
    rows = [ln.split("\t") for ln in path.read_text(encoding="utf-8").splitlines() if "\t" in ln]
    rows = rows[1:] if rows else rows  # drop the header
    confidences, correct = [], []
    for r in rows:
        if len(r) < 2:
            continue
        try:
            confidences.append(float(r[0].strip()))
            correct.append(1 if r[1].strip() in ("1", "true", "True") else 0)
        except ValueError:
            continue
    if not confidences:
        print("no usable rows (need a header then <confidence 0..1> TAB <correct 1|0>)", file=sys.stderr)
        return 2
    ece, bins = expected_calibration_error(confidences, correct, n_bins)
    print(f"Expected Calibration Error (ECE) = {ece:.4f}   N={len(confidences)} predictions, {n_bins} bins")
    print(f"  (0 = perfectly calibrated; a stated confidence matches observed accuracy. Only trust this over")
    print(f"   REAL-posterior confidences — the offline CTC 0.90 heuristic makes it meaningless.)")
    print("  bin            count  mean_conf  accuracy   gap")
    for bn in bins:
        print(
            f"  [{bn['lo']:.2f},{bn['hi']:.2f})  {bn['count']:5d}    {bn['mean_conf']:.3f}     "
            f"{bn['accuracy']:.3f}    {bn['gap']:.3f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
