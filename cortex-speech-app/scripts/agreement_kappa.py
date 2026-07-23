#!/usr/bin/env python3
"""Inter-annotator agreement (Cohen's kappa) for the Gold-Marathon calibration set.

With only ~3 human-verified segments today, the GOLD SET's own label noise — not the model — caps every
accuracy number the app can honestly report (a gold set built at kappa~=0.80 caps measurable model
accuracy at ~0.80; see docs/ASR_TECH_SCAN_2026-07-23.md §4). Before quoting any CER as "the model's
error", the owner should double-annotate a subset and report the agreement. This computes Cohen's kappa
for the realistic first case — TWO annotators labelling the same items (e.g. accept / edit / reject).

For >2 annotators use Krippendorff's alpha instead (NOT implemented here — YAGNI until a second annotator
exists; kappa is the 2-rater case the marathon starts with).

Pure stdlib: the kappa math is a dependency-free function unit-tested against the textbook kappa=0.40
example, so it always runs on the policy runner.

Usage:  python scripts/agreement_kappa.py <annotations.tsv>
        # TSV with a header row then two label columns per item: <rater_a>\t<rater_b>
"""

from __future__ import annotations

import sys
from pathlib import Path

# Landis & Koch (1977) interpretation bands for kappa.
_BANDS = [
    (0.81, "almost perfect"),
    (0.61, "substantial"),
    (0.41, "moderate"),
    (0.21, "fair"),
    (0.01, "slight"),
    (-1.0, "poor / worse than chance"),
]


def cohen_kappa(a, b) -> float:
    """Cohen's kappa for two equal-length sequences of categorical labels.

    kappa = (po - pe) / (1 - pe), where po is observed agreement and pe is chance agreement. Returns 1.0
    when raters agree on every item even if only one label was used (pe==1); returns 0.0 for the
    degenerate single-label-but-disagreeing case (kappa is otherwise undefined there).
    """
    if len(a) != len(b):
        raise ValueError(f"rater label sequences must be equal length ({len(a)} vs {len(b)})")
    n = len(a)
    if n == 0:
        raise ValueError("no items to score")
    po = sum(1 for x, y in zip(a, b) if x == y) / n
    labels = set(a) | set(b)
    pe = sum((a.count(k) / n) * (b.count(k) / n) for k in labels)
    if pe >= 1.0:
        return 1.0 if po >= 1.0 else 0.0
    return (po - pe) / (1.0 - pe)


def interpret(kappa: float) -> str:
    for threshold, label in _BANDS:
        if kappa >= threshold:
            return label
    return "poor / worse than chance"


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python scripts/agreement_kappa.py <annotations.tsv>  (header + two label columns)")
        return 2
    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"annotations file not found: {path}", file=sys.stderr)
        return 2
    rows = [ln.split("\t") for ln in path.read_text(encoding="utf-8").splitlines() if "\t" in ln]
    rows = rows[1:] if rows else rows  # drop the header row
    a = [r[0].strip() for r in rows if len(r) >= 2]
    b = [r[1].strip() for r in rows if len(r) >= 2]
    if not a:
        print("no annotated rows (need a header then two tab-separated label columns)", file=sys.stderr)
        return 2
    k = cohen_kappa(a, b)
    print(f"Cohen's kappa = {k:.4f}  ({interpret(k)})   N={len(a)} items, 2 raters")
    print(f"  => the gold set's label ceiling: measurable model accuracy is capped near this agreement.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
