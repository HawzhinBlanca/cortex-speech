#!/usr/bin/env python3
"""MAPSSWE (Matched-Pairs Sentence-Segment Word Error) significance test between two engines'
per-clip result TSVs (columns: char_dist, char_ref_len, word_dist, word_ref_len; identical manifest
row order — the format written by scorecard_7b.py / scorecard_seamless.py / ckb_scorecard_on_gold).

The standard NIST matched-pairs z-test: per segment i, Z_i = errors_A(i) - errors_B(i);
z = mean(Z) / sqrt(var(Z)/n); two-tailed p from the normal approximation (n=922 >> 50, so the
normal approximation is sound). Run on word errors (the charter's WER basis) and char errors.

Usage: python scripts/mapsswe_compare.py <A_results.tsv> <B_results.tsv> [labelA] [labelB]
Exit 0 always (it reports; gating is the caller's choice).
"""
import math
import sys


def load(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        header = f.readline().rstrip("\n").split("\t")
        idx = {c: i for i, c in enumerate(header)}
        for line in f:
            p = line.rstrip("\n").split("\t")
            if len(p) < len(header):
                continue
            rows.append(
                (int(p[idx["char_dist"]]), int(p[idx["char_ref_len"]]), int(p[idx["word_dist"]]), int(p[idx["word_ref_len"]]))
            )
    return rows


def mapsswe(diffs):
    n = len(diffs)
    mean = sum(diffs) / n
    var = sum((d - mean) ** 2 for d in diffs) / (n - 1)
    if var == 0:
        return mean, float("inf") if mean else 0.0, 0.0 if mean else 1.0
    z = mean / math.sqrt(var / n)
    # two-tailed normal p-value via erfc
    p = math.erfc(abs(z) / math.sqrt(2))
    return mean, z, p


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    a, b = load(sys.argv[1]), load(sys.argv[2])
    la = sys.argv[3] if len(sys.argv) > 3 else "A"
    lb = sys.argv[4] if len(sys.argv) > 4 else "B"
    if len(a) != len(b):
        print(f"ROW MISMATCH: {la}={len(a)} rows vs {lb}={len(b)} rows — segments must pair 1:1; refusing.")
        return 1
    n = len(a)
    for name, ei, ri in (("word", 2, 3), ("char", 0, 1)):
        diffs = [ra[ei] - rb[ei] for ra, rb in zip(a, b)]
        mean, z, p = mapsswe(diffs)
        ra = sum(r[ei] for r in a) / max(sum(r[ri] for r in a), 1)
        rb_ = sum(r[ei] for r in b) / max(sum(r[ri] for r in b), 1)
        verdict = f"{la} better" if mean < 0 else f"{lb} better" if mean > 0 else "tied"
        print(
            f"MAPSSWE {name}: {la} {ra * 100:.2f}% vs {lb} {rb_ * 100:.2f}%  "
            f"mean diff/seg = {mean:+.3f}  z = {z:+.2f}  p = {p:.3e}  N={n}  -> {verdict}"
            + ("  (SIGNIFICANT p<0.05)" if p < 0.05 else "  (not significant)")
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
