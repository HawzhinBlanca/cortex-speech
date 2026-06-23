#!/usr/bin/env python3
"""Compute micro CER/WER + bootstrap CI + per-group fairness from a ckb scorecard results file.

Consumes the per-clip TSV emitted by the `ckb_scorecard_on_gold` test (env CORTEX_GOLD_RESULTS):
    gender<TAB>age<TAB>char_dist<TAB>char_ref_len<TAB>word_dist<TAB>word_ref_len

micro CER = sum(char_dist) / sum(char_ref_len)  (ratio-of-sums, Bisani & Ney 2004).
95% CI via the utterance bootstrap (resample clips with replacement, seeded for reproducibility).
Also reports per-gender / per-age micro CER (the dataset's metadata) for a fairness slice.

Usage:  python scripts/scorecard_stats.py <results.tsv> [bootstrap_samples]
"""
import csv
import random
import sys
from collections import defaultdict

# results columns: gender, age, script, char_dist, char_ref_len, word_dist, word_ref_len
CHAR_DIST, CHAR_REF, WORD_DIST, WORD_REF = 3, 4, 5, 6


def micro(rows, dist_idx, ref_idx):
    d = sum(r[dist_idx] for r in rows)
    rl = sum(r[ref_idx] for r in rows)
    return d / rl if rl else 0.0


def bootstrap_ci(rows, dist_idx, ref_idx, b=2000, seed=1234):
    rng = random.Random(seed)
    n = len(rows)
    vals = []
    for _ in range(b):
        samp = [rows[rng.randrange(n)] for _ in range(n)]
        vals.append(micro(samp, dist_idx, ref_idx))
    vals.sort()
    return vals[int(0.025 * (len(vals) - 1))], vals[int(0.975 * (len(vals) - 1))]


def main():
    if len(sys.argv) < 2:
        print("usage: scorecard_stats.py <results.tsv> [bootstrap_samples]", file=sys.stderr)
        return 2
    path = sys.argv[1]
    b = int(sys.argv[2]) if len(sys.argv) > 2 else 2000

    rows = []
    with open(path, encoding="utf-8", newline="") as f:
        reader = csv.reader(f, delimiter="\t")
        next(reader, None)  # header
        for row in reader:
            if len(row) < 7:
                continue
            rows.append((row[0], row[1], row[2], int(row[3]), int(row[4]), int(row[5]), int(row[6])))

    n = len(rows)
    cer = micro(rows, CHAR_DIST, CHAR_REF)
    wer = micro(rows, WORD_DIST, WORD_REF)
    lo, hi = bootstrap_ci(rows, CHAR_DIST, CHAR_REF, b)

    print(f"N = {n}")
    print(f"micro CER = {cer * 100:.2f}%   95% CI [{lo * 100:.2f}%, {hi * 100:.2f}%]  ({b} bootstrap)")
    print(f"micro WER = {wer * 100:.2f}%")

    by_g = defaultdict(list)
    for r in rows:
        by_g[r[0] or "(unknown)"].append(r)
    print("\nPer-gender micro CER:")
    cers = {}
    for g, rs in sorted(by_g.items(), key=lambda x: -len(x[1])):
        c = micro(rs, CHAR_DIST, CHAR_REF)
        cers[g] = c
        print(f"  {g:<10} N={len(rs):<5} CER={c * 100:.2f}%")
    if len(cers) >= 2:
        disparity = max(cers.values()) - min(cers.values())
        print(f"  -> max-min gender CER disparity: {disparity * 100:.2f} pts")

    by_s = defaultdict(list)
    for r in rows:
        by_s[r[2] or "(unknown)"].append(r)
    print("\nOutput-script split (does the model emit Kurdish vs romanize to Latin?):")
    for s, rs in sorted(by_s.items(), key=lambda x: -len(x[1])):
        frac = 100.0 * len(rs) / n
        print(f"  {s:<8} N={len(rs):<5} ({frac:.0f}% of clips)  CER={micro(rs, CHAR_DIST, CHAR_REF) * 100:.2f}%")

    by_a = defaultdict(list)
    for r in rows:
        by_a[r[1] or "(unknown)"].append(r)
    print("\nPer-age micro CER (N >= 10):")
    for a, rs in sorted(by_a.items(), key=lambda x: -len(x[1])):
        if len(rs) >= 10:
            print(f"  {a:<10} N={len(rs):<5} CER={micro(rs, CHAR_DIST, CHAR_REF) * 100:.2f}%")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
