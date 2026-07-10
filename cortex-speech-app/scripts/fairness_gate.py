#!/usr/bin/env python3
"""fairness-gender-age gate (verify_10 Tier 3, WS4).

Enforces a max-min micro-CER *disparity budget* across demographic subgroups, using the
committed aggregate fairness scorecard (docs/fairness_scorecard.json). No audio, no model,
no exe — pure data, so it runs on a clean checkout and in CI.

Honesty rules baked in:
  * Only subgroups with n >= n_floor are GATED. Smaller cells are printed but not gated,
    because EVAL.md flags them as directional-not-conclusive; gating on noise would
    false-RED on a future run. An axis with < 2 adequately-powered groups is reported as
    UNDERPOWERED (not gated), not silently passed.
  * The gate is non-vacuous: it FAILs (exit 2) if NO axis could be gated at all.
  * Every number is the committed measurement; this script computes disparity, it does not
    invent CER.

Usage:
    python scripts/fairness_gate.py [scorecard.json]   # default: docs/fairness_scorecard.json
    python scripts/fairness_gate.py --selftest         # logic self-check, no external file
"""
import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SCORECARD = REPO_ROOT / "docs" / "fairness_scorecard.json"


def evaluate(card):
    """Return (ok, gated_any, lines) for a loaded scorecard dict.

    ok        -> False iff some adequately-powered axis exceeds the budget.
    gated_any -> True iff at least one axis had >= 2 groups with n >= n_floor.
    """
    n_floor = card["n_floor"]
    budget = card["budget_pts"]
    lines = [f"fairness gate: n_floor={n_floor}  budget={budget:.2f} pts  ({card['metric']}, {card['model']})"]
    ok = True
    gated_any = False

    for axis, groups in card["groups"].items():
        rows = sorted(groups.items(), key=lambda kv: -kv[1]["n"])
        powered = {g: v for g, v in groups.items() if v["n"] >= n_floor}
        lines.append(f"\n[{axis}]")
        for g, v in rows:
            tag = "gated" if v["n"] >= n_floor else f"reported (n<{n_floor})"
            lines.append(f"  {g:<10} n={v['n']:<5} CER={v['cer']:.2f}%   [{tag}]")

        if len(powered) < 2:
            names = ", ".join(f"{g}(n={v['n']})" for g, v in rows) or "(none)"
            lines.append(f"  -> UNDERPOWERED: <2 groups with n>={n_floor} ({names}); axis reported, not gated")
            continue

        gated_any = True
        cers = {g: v["cer"] for g, v in powered.items()}
        hi_g, lo_g = max(cers, key=cers.get), min(cers, key=cers.get)
        disparity = cers[hi_g] - cers[lo_g]
        verdict = "PASS" if disparity <= budget else "FAIL"
        if disparity > budget:
            ok = False
        lines.append(
            f"  -> max-min disparity {disparity:.2f} pts "
            f"({hi_g} {cers[hi_g]:.2f}% - {lo_g} {cers[lo_g]:.2f}%)  budget {budget:.2f}  [{verdict}]"
        )
    return ok, gated_any, lines


def _selftest():
    base = {"metric": "micro_cer", "model": "test", "n_floor": 50, "budget_pts": 10.0}

    within = {**base, "groups": {"age": {"a": {"n": 100, "cer": 20.0}, "b": {"n": 100, "cer": 25.0}}}}
    ok, gated, _ = evaluate(within)
    assert ok and gated, "5pt disparity under 10pt budget must PASS and be gated"

    over = {**base, "groups": {"age": {"a": {"n": 100, "cer": 20.0}, "b": {"n": 100, "cer": 45.0}}}}
    ok, gated, _ = evaluate(over)
    assert (not ok) and gated, "25pt disparity over 10pt budget must FAIL"

    # Small cells excluded -> underpowered axis is not gated (and not a failure).
    underpowered = {**base, "groups": {"gender": {"m": {"n": 375, "cer": 30.0}, "f": {"n": 25, "cer": 10.0}}}}
    ok, gated, _ = evaluate(underpowered)
    assert ok and not gated, "only one group >= n_floor -> underpowered, not gated, not a failure"

    # A borderline group exactly at n_floor is gated (>=, not >).
    edge = {**base, "groups": {"age": {"a": {"n": 50, "cer": 20.0}, "b": {"n": 50, "cer": 22.0}}}}
    ok, gated, _ = evaluate(edge)
    assert ok and gated, "n exactly == n_floor must count as adequately powered"

    print("fairness_gate selftest passed")


def main(argv):
    if "--selftest" in argv:
        _selftest()
        return 0

    path = Path(next((a for a in argv[1:] if not a.startswith("-")), DEFAULT_SCORECARD))
    card = json.loads(path.read_text(encoding="utf-8"))
    ok, gated_any, lines = evaluate(card)
    print("\n".join(lines))

    if not gated_any:
        print("\n-> INCONCLUSIVE: no axis had >=2 groups with n>=n_floor; cannot gate fairness on this data.")
        return 2
    if not ok:
        print("\n-> RED: an adequately-powered subgroup exceeds the disparity budget.")
        return 1
    print("\n-> GREEN: all adequately-powered subgroups within the disparity budget.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
