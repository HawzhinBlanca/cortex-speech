#!/usr/bin/env python3
"""The promotion gate: prove a challenger is better BEFORE it may replace the champion.

Phase 3 of docs/PLAN_TRUE_10.md, and the last clause of the governing rule:

    Record every decision; include every eligible latest human label exactly once in the next
    immutable batch; PROVE THE CHALLENGER IS BETTER BEFORE PROMOTION.

Reads two per-clip scorecards produced on the SAME evaluation manifest — the champion's and the
challenger's, in the `clip_index/char_dist/char_ref_len/word_dist/word_ref_len/norm_basis` format
`scorecard_7b.py` and `scorecard_finetuned.py` already emit — and answers one question: may this
challenger be promoted?

    python scripts/promotion_gate.py CHAMPION.tsv CHALLENGER.tsv [--slices slices.tsv] [--out verdict.json]

Exit 0 = PROMOTE, 1 = REJECT, 2 = the comparison itself is invalid. A verdict is ALWAYS written.

WHY IT REFUSES BY DEFAULT
-------------------------
The owner's model lock says the champion is fixed infrastructure; the only thing that may ever
displace it is evidence. So every arm below fails CLOSED — a comparison that cannot be made honestly
is a REJECT, never a shrug. In particular an UNPAIRED comparison is refused outright: if the two
engines skipped different clips, the aggregate difference is partly a difference of test sets, and
that is exactly the shape of a flattering number.

Slices are the second half. An overall CER win that hides a regression on one dialect, one speaker,
or the noisy clips is not an improvement for the people whose audio regressed — and in a corpus where
one recording is 94.7 % of the labeled duration, an overall average is dominated by one voice.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

# An improvement smaller than this is noise dressed as progress; the champion stays.
MIN_CER_IMPROVEMENT = 0.001  # 0.1 percentage points of CER
# MAPSSWE two-sided p below this is "not chance". 0.05 with the significance test the repo already
# uses (scripts/mapsswe_compare.py, Gillick & Cox).
MAX_P_VALUE = 0.05
# A protected slice may drift by this much before it counts as a regression. Slices are small, so
# some movement is sampling; a real regression is bigger than the slice's own noise.
SLICE_REGRESSION_TOLERANCE = 0.02  # 2 CER points


def load_scorecard(path: Path) -> tuple[dict[int, tuple[int, int, int, int]], str | None]:
    """clip_index -> (char_dist, char_ref_len, word_dist, word_ref_len), plus the normalization basis."""
    rows: dict[int, tuple[int, int, int, int]] = {}
    basis: str | None = None
    with path.open(encoding="utf-8") as handle:
        header = handle.readline().rstrip("\n").split("\t")
        index = {name: i for i, name in enumerate(header)}
        required = ("clip_index", "char_dist", "char_ref_len", "word_dist", "word_ref_len")
        missing = [c for c in required if c not in index]
        if missing:
            raise SystemExit(f"{path.name} is missing column(s) {missing} — not a per-clip scorecard")
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < len(header):
                continue
            if "norm_basis" in index:
                basis = parts[index["norm_basis"]]
            rows[int(parts[index["clip_index"]])] = (
                int(parts[index["char_dist"]]),
                int(parts[index["char_ref_len"]]),
                int(parts[index["word_dist"]]),
                int(parts[index["word_ref_len"]]),
            )
    return rows, basis


def micro_cer(rows: dict[int, tuple[int, int, int, int]], clips=None) -> float | None:
    """Ratio-of-sums CER over `clips` (all rows when None). None when there is nothing to score."""
    keys = rows.keys() if clips is None else [c for c in clips if c in rows]
    dist = sum(rows[c][0] for c in keys)
    ref = sum(rows[c][1] for c in keys)
    return (dist / ref) if ref else None


def micro_wer(rows: dict[int, tuple[int, int, int, int]], clips=None) -> float | None:
    keys = rows.keys() if clips is None else [c for c in clips if c in rows]
    dist = sum(rows[c][2] for c in keys)
    ref = sum(rows[c][3] for c in keys)
    return (dist / ref) if ref else None


def mapsswe_p(champion, challenger, clips) -> float | None:
    """Two-sided MAPSSWE p-value over the PAIRED clips (Gillick & Cox), or None if undefined.

    Same statistic as scripts/mapsswe_compare.py, computed here on the paired subset so the gate does
    not have to shell out and re-read the files.
    """
    diffs = [champion[c][0] - challenger[c][0] for c in clips]
    n = len(diffs)
    if n < 2:
        return None
    mean = sum(diffs) / n
    var = sum((d - mean) ** 2 for d in diffs) / (n - 1)
    if var <= 0:
        # Every clip moved identically (usually: not at all). No evidence of a difference.
        return None if mean == 0 else 0.0
    z = mean / math.sqrt(var / n)
    return math.erfc(abs(z) / math.sqrt(2.0))


def load_slices(path: Path) -> dict[str, list[int]]:
    """`slice_name<TAB>clip_index` rows -> {slice: [clip_index, ...]}.

    Protected slices are supplied rather than inferred: what deserves protection (a dialect, a
    speaker bucket, the noisy clips) is a judgement about the corpus, not something a gate should
    guess.
    """
    slices: dict[str, list[int]] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            try:
                slices.setdefault(parts[0], []).append(int(parts[1]))
            except ValueError:
                continue
    return slices


def decide(champion, challenger, champion_basis, challenger_basis, slices) -> dict:
    """The whole decision, as data. Pure, so the tests can pin every arm without files."""
    reasons: list[str] = []
    verdict = "PROMOTE"

    def refuse(reason: str, invalid: bool = False) -> None:
        nonlocal verdict
        reasons.append(reason)
        verdict = "INVALID" if invalid else ("INVALID" if verdict == "INVALID" else "REJECT")

    # 1. The two runs must be comparable at all.
    if champion_basis and challenger_basis and champion_basis != challenger_basis:
        refuse(
            f"normalization basis differs ({champion_basis} vs {challenger_basis}) — the two CERs are "
            "not on the same scale and their difference means nothing",
            invalid=True,
        )
    paired = sorted(set(champion) & set(challenger))
    only_champion = sorted(set(champion) - set(challenger))
    only_challenger = sorted(set(challenger) - set(champion))
    if not paired:
        refuse("no clip was scored by both engines — there is nothing to compare", invalid=True)
    if only_champion or only_challenger:
        refuse(
            f"unpaired evaluation: {len(only_champion)} clip(s) only the champion scored and "
            f"{len(only_challenger)} only the challenger scored. An aggregate over different clip sets "
            "measures the test set as much as the model",
            invalid=True,
        )

    champion_cer = micro_cer(champion, paired)
    challenger_cer = micro_cer(challenger, paired)
    champion_wer = micro_wer(champion, paired)
    challenger_wer = micro_wer(challenger, paired)
    p_value = mapsswe_p(champion, challenger, paired) if paired else None

    # 2. It has to actually be better, by more than noise.
    if champion_cer is None or challenger_cer is None:
        refuse("CER is undefined (no reference characters) — nothing was measured", invalid=True)
    else:
        improvement = champion_cer - challenger_cer
        if improvement <= 0:
            refuse(f"CER did not improve ({champion_cer:.4f} -> {challenger_cer:.4f})")
        elif improvement < MIN_CER_IMPROVEMENT:
            refuse(
                f"CER improved by only {improvement:.4f} (< {MIN_CER_IMPROVEMENT}) — inside the noise "
                "this corpus can produce"
            )
        elif p_value is None:
            refuse("the improvement has no significance estimate — too few paired clips to tell")
        elif p_value > MAX_P_VALUE:
            refuse(f"the improvement is not significant (MAPSSWE p={p_value:.4f} > {MAX_P_VALUE})")

    # 3. No protected group may pay for the average.
    slice_report = []
    for name, clips in sorted(slices.items()):
        in_both = [c for c in clips if c in champion and c in challenger]
        before = micro_cer(champion, in_both)
        after = micro_cer(challenger, in_both)
        entry = {"slice": name, "clips": len(in_both), "champion_cer": before, "challenger_cer": after}
        slice_report.append(entry)
        if before is None or after is None:
            refuse(f"slice '{name}' could not be scored ({len(in_both)} paired clip(s)) — cannot confirm it is safe")
        elif after - before > SLICE_REGRESSION_TOLERANCE:
            refuse(
                f"slice '{name}' REGRESSED ({before:.4f} -> {after:.4f}, +{after - before:.4f}) — an "
                "overall win that costs one dialect, speaker or condition is not an improvement for them"
            )

    if verdict == "PROMOTE":
        reasons.append(
            f"CER {champion_cer:.4f} -> {challenger_cer:.4f} on {len(paired)} paired clip(s), "
            f"MAPSSWE p={p_value:.4f}, no protected slice regressed"
        )

    return {
        "verdict": verdict,
        "paired_clips": len(paired),
        "champion_cer": champion_cer,
        "challenger_cer": challenger_cer,
        "champion_wer": champion_wer,
        "challenger_wer": challenger_wer,
        "cer_improvement": (champion_cer - challenger_cer) if (champion_cer is not None and challenger_cer is not None) else None,
        "mapsswe_p": p_value,
        "norm_basis": champion_basis or challenger_basis,
        "slices": slice_report,
        "thresholds": {
            "min_cer_improvement": MIN_CER_IMPROVEMENT,
            "max_p_value": MAX_P_VALUE,
            "slice_regression_tolerance": SLICE_REGRESSION_TOLERANCE,
        },
        "reasons": reasons,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Decide whether a challenger may replace the champion")
    parser.add_argument("champion_tsv", type=Path)
    parser.add_argument("challenger_tsv", type=Path)
    parser.add_argument("--slices", type=Path, help="slice_name<TAB>clip_index rows for protected groups")
    parser.add_argument("--snapshot-id", help="the dataset snapshot the challenger was trained on")
    parser.add_argument("--out", type=Path, help="where to write the verdict JSON")
    args = parser.parse_args()

    champion, champion_basis = load_scorecard(args.champion_tsv)
    challenger, challenger_basis = load_scorecard(args.challenger_tsv)
    slices = load_slices(args.slices) if args.slices else {}

    report = decide(champion, challenger, champion_basis, challenger_basis, slices)
    report["champion_scorecard"] = str(args.champion_tsv)
    report["challenger_scorecard"] = str(args.challenger_tsv)
    if args.snapshot_id:
        report["snapshot_id"] = args.snapshot_id

    out = args.out or args.challenger_tsv.with_name("promotion_verdict.json")
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")

    print(f"PROMOTION GATE: {report['verdict']}", flush=True)
    for reason in report["reasons"]:
        print(f"  - {reason}", flush=True)
    for entry in report["slices"]:
        before, after = entry["champion_cer"], entry["challenger_cer"]
        shown = "unscored" if before is None or after is None else f"{before:.4f} -> {after:.4f}"
        print(f"    slice {entry['slice']}: {shown} ({entry['clips']} clips)", flush=True)
    print(f"  verdict written to {out}", flush=True)

    return {"PROMOTE": 0, "REJECT": 1}.get(report["verdict"], 2)


if __name__ == "__main__":
    sys.exit(main())
