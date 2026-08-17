#!/usr/bin/env python3
"""Score a Gemini hypothesis TSV against the frozen ckb gold set — comparable to the champion BY
CONSTRUCTION.

The transcription itself is done by `src-tauri/src/bin/gemini_asr_bench.rs`, which writes ONLY
`<wav_path>\t<reference>\t<hypothesis>` rows. This script supplies the metric, and it deliberately
imports the SAME primitives `scorecard_7b.py` uses — `_NORM` (NFC + lower + whitespace collapse +
Arabic-digit folding), character edit distance over the whitespace-KEEPING normalized string, micro
CER as a ratio-of-sums, and the seed-42 utterance bootstrap. A second implementation of "the metric"
is how a comparison starts lying: this repo has fixed that mirror-drift class repeatedly, so the
comparison must share code, not merely share intent.

    python scripts/scorecard_gemini.py <hypotheses.tsv> [bootstrap=3000]

Reports micro CER + 95% CI + WER at the n actually scored, and states n explicitly — a benchmark run
that lost clips to provider errors must never present itself as a full-set number.
"""
import random
import sys
import unicodedata

from asr_metrics import bootstrap_ci as boot_ci
from asr_metrics import micro as micro_rate
from asr_metrics import word_pair

# Imported, not re-implemented: the champion's own normalization and distance.
from scorecard_7b import _NORM, edit_distance


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    tsv = sys.argv[1]
    n_boot = int(sys.argv[2]) if len(sys.argv) > 2 else 3000

    rows = []
    for line in open(tsv, encoding="utf-8"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 3 and parts[1].strip():
            rows.append((parts[0], parts[1], parts[2]))
    if not rows:
        print(f"no scorable rows in {tsv}")
        return 1

    per_clip, word_clip, empty_hyp = [], [], 0
    for _wav, ref, hyp in rows:
        r, h = _NORM(ref), _NORM(hyp)
        if not r:
            continue  # zero-reference clip drops out of the ratio-of-sums (matches eval.rs)
        if not h:
            # Counted and reported, never silently scored: an empty hypothesis is a 100%-CER clip
            # and inflating or hiding it would both be dishonest.
            empty_hyp += 1
        per_clip.append((edit_distance(list(r), list(h)), len(r)))
        word_clip.append(word_pair(r, h))

    n = len(per_clip)
    dists = [d for d, _ in per_clip]
    refs = [r for _, r in per_clip]
    micro = sum(dists) / max(sum(refs), 1)

    rng = random.Random(42)
    boots = []
    for _ in range(n_boot):
        sample = [rng.randrange(n) for _ in range(n)]
        boots.append(sum(dists[k] for k in sample) / max(sum(refs[k] for k in sample), 1))
    boots.sort()
    lo = boots[int(0.025 * len(boots))]
    hi = boots[int(0.975 * len(boots)) - 1]

    wer = micro_rate(word_clip)
    wlo, whi = boot_ci(word_clip, n_boot, 42)

    print(f"file          : {tsv}")
    print(f"scored clips  : {n}")
    if empty_hyp:
        print(f"empty hypotheses included as full errors: {empty_hyp}")
    print(f"micro CER     : {micro * 100:.2f}%   95% CI [{lo * 100:.2f}, {hi * 100:.2f}]")
    print(f"micro WER     : {wer * 100:.2f}%   95% CI [{wlo * 100:.2f}, {whi * 100:.2f}]")
    print(f"bootstrap     : {n_boot} resamples, seed 42 (same recipe as scorecard_7b.py)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
