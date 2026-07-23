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
    if n < 2:
        # A matched-pairs test needs >= 2 paired segments; with one the sample variance is undefined (the
        # /(n-1) below divides by zero). Report non-significant honestly rather than crash or fabricate a p.
        return (sum(diffs) / n if n else 0.0), float("nan"), 1.0
    mean = sum(diffs) / n
    var = sum((d - mean) ** 2 for d in diffs) / (n - 1)
    if var == 0:
        if mean == 0:
            return mean, 0.0, 1.0  # every paired diff identical and zero -> systems indistinguishable
        # Constant NON-zero gap: the z-statistic mean/(s/sqrt(n)) has a zero denominator and is UNDEFINED —
        # NOT infinitely significant (the old `inf, 0.0` printed a fabricated "SIGNIFICANT p<0.05"). A uniform
        # gap over a handful of segments is weak evidence. Fall back to the two-sided sign test: all n diffs
        # share one sign, so p = 2*0.5^n (n=3 -> 0.25, i.e. NOT significant). Mirrors the canonical Rust
        # reference significance.rs:182-193, which fixed this exact p=0.0 fabrication (and its test
        # mapsswe_constant_nonzero_gap_is_not_falsely_significant).
        return mean, float("nan"), min(1.0, 2.0 * 0.5 ** n)
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
