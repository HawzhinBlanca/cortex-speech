#!/usr/bin/env python3
"""P2.1 — shared, unit-tested ASR metric helpers for the M1 scorecards.

Provides the WORD-level (WER) counterpart to the char-level (CER) math the scorecards already compute
inline, plus the identical seeded ratio-of-sums bootstrap CI. The scorecards keep their existing CER
path byte-identical (so the published 21.00% / 29.40% stay comparable) and use these helpers only for
the newly-added WER and its CI. Everything here is pure and deterministic.
"""

from __future__ import annotations

import random


def edit_distance(a: list, b: list) -> int:
    """Levenshtein distance over two sequences (chars for CER, tokens for WER)."""
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (0 if ca == cb else 1)))
        prev = cur
    return prev[-1]


def word_pair(ref_norm: str, hyp_norm: str) -> tuple[int, int]:
    """(word_edit_distance, word_ref_len) for one already-normalized ref/hyp pair."""
    r_words = ref_norm.split()
    h_words = hyp_norm.split()
    return edit_distance(r_words, h_words), len(r_words)


def micro(per_clip: list[tuple[int, int]]) -> float:
    """Micro rate = Σ edit-distance / Σ ref-length (ratio-of-sums, matching eval.rs)."""
    total_dist = sum(d for d, _ in per_clip)
    total_ref = sum(r for _, r in per_clip)
    return total_dist / max(total_ref, 1)


def bootstrap_ci(per_clip: list[tuple[int, int]], n_boot: int = 3000, seed: int = 42) -> tuple[float, float]:
    """Seeded (Bisani & Ney) utterance bootstrap 95% CI of the micro rate. Empty -> (0, 0)."""
    n = len(per_clip)
    if n == 0:
        return 0.0, 0.0
    dists = [d for d, _ in per_clip]
    refs = [r for _, r in per_clip]
    rng = random.Random(seed)
    boots = []
    for _ in range(n_boot):
        idx = [rng.randrange(n) for _ in range(n)]
        sd = sum(dists[k] for k in idx)
        sr = sum(refs[k] for k in idx)
        boots.append(sd / max(sr, 1))
    boots.sort()
    lo = boots[int(0.025 * len(boots))]
    hi = boots[int(0.975 * len(boots)) - 1]
    return lo, hi


def rtf(total_processing_s: float, total_audio_s: float) -> float | None:
    """Real-time factor = processing time / audio duration. None when audio duration is unknown (0)."""
    if total_audio_s <= 0:
        return None
    return total_processing_s / total_audio_s
