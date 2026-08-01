#!/usr/bin/env python3
"""Criterion wall-clock regression gate against a COMMITTED baseline.

WHY THIS EXISTS. `AGENT_CHARTER.md` requires "criterion benches gated on every PR with a >5%
wall-clock regression budget via github-action-benchmark against a committed baseline". Three benches
(normalizer, diff, audio) have existed for a long time and NOTHING ran them — verified against
verify_10.py, every workflow and the Makefile. A bench nobody runs is not a budget.

WHAT THIS IS, AND WHAT IT IS NOT. This enforces the BUDGET on the reference machine, which is where
the charter's latency numbers are defined anyway ("on the named reference machine"). It does NOT
satisfy the github-action-benchmark-on-every-PR clause: that needs CI that cannot be run or verified
from here, and half-wiring a gate whose green nobody can reproduce is the artefact iteration 235
spent its time deleting. The clause stays open and named in the ledger; this makes it smaller by
producing the committed baseline it would consume.

NO VACUOUS PASS. If the run parses zero benchmarks — cargo missing, a bench target renamed, an
output-format change — this FAILS rather than reporting "0 regressions". That exact shape (a gate
sailing through an empty result set) is what the fuzz leg was fixed for on 2026-07-26.

THE BASELINE MUST BE MEASURED IN THE CONDITION THE GATE RUNS IN. Learned the hard way on
2026-08-02: the first baseline was taken with the app STOPPED because that was the quiet, convenient
state, and the first real run — inside a verify-10 sweep, app UP as always — reported five
regressions of 1.16x to 1.41x. Not one line of code had changed; the machine was simply busier. The
gate was right and the baseline was wrong. So regenerate with the app RUNNING, which is what a sweep
looks like, and accept the wider thresholds that come with it: a tighter budget measured under
conditions the gate never sees is not a stricter gate, it is a false one.

Usage:
  python scripts/bench_gate.py            # measure and compare against the committed baseline
  python scripts/bench_gate.py --update   # re-measure and REWRITE the baseline (deliberate act)
  python scripts/bench_gate.py --runs 3   # repeat, report spread, and use the FASTEST per bench
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
MANIFEST = APP / "src-tauri" / "Cargo.toml"
BASELINE = APP / "docs" / "bench_baseline.json"

# The charter's number, and the FLOOR for every bench — never the whole story.
#
# Measured 2026-08-02 on the reference machine, 3 runs of 12 benches, APP RUNNING (the condition a
# verify-10 sweep is in — see the docstring for why that matters): run-to-run spread was min 1.5%,
# median 5.6%, MAX 61.3%. For reference the same measurement with the app stopped gave max 25.9%, so
# roughly half this noise is the app itself, and the gate has to live with it because that is when it
# runs. A flat 5% budget would fire on that noise nearly every sweep — and a gate that cries wolf gets
# muted, which is worse than no gate at all.
#
# So the threshold is per-bench: the charter's 5% where the machine really is that quiet — measured,
# exactly ONE of the twelve — and 2x the measured spread everywhere else. That honours the budget
# wherever the hardware permits and states the real number where it does not, instead of applying 5%
# everywhere and calling the resulting flakes "known". The gate PRINTS the split on every run, so
# these numbers cannot drift away from what is actually enforced.
BUDGET = 0.05
NOISE_MULTIPLIER = 2.0

# Past this, a "threshold" stops being a budget. Measured app-up, 11 of 12 benches derive a limit of
# 1.49x or tighter; ONE - audio/waveform_decode_16000_samples, the shortest at ~71us - needs 2.23x,
# because at that duration scheduler jitter dominates the measurement. Giving it a 2.23x pass-anything
# limit would let a genuine 2x regression through while LOOKING gated, which is the exact dishonesty
# this file exists to avoid. So it is reported as NOT ENFORCEABLE, by name, on every single run.
MAX_THRESHOLD = 1.50


def threshold_for(name: str, spread: dict[str, float]) -> float | None:
    """Allowed ratio before `name` counts as a regression, or None when the noise makes it unenforceable."""
    limit = max(1.0 + BUDGET, 1.0 + NOISE_MULTIPLIER * spread.get(name, 0.0))
    return None if limit > MAX_THRESHOLD else limit

# `cargo bench -- --output-format bencher` emits: "test <name> ... bench:  1234 ns/iter (+/- 56)"
BENCH_LINE = re.compile(r"^test\s+(?P<name>\S+)\s+\.\.\.\s+bench:\s+(?P<ns>[\d,]+)\s+ns/iter")


# ONLY the criterion targets. A bare `cargo bench` also runs the lib's default libtest harness, which
# rejects `--output-format` outright ("error: Unrecognized option: 'output-format'") and takes the whole
# run down with it — so the gate would fail for a reason that has nothing to do with performance.
BENCHES = ["normalizer", "diff", "audio"]


def measure() -> dict[str, int]:
    """One full bench pass, as a {name: ns_per_iter} map."""
    cmd = ["cargo", "bench", "--manifest-path", str(MANIFEST)]
    for b in BENCHES:
        cmd += ["--bench", b]
    cmd += ["--", "--output-format", "bencher"]
    # A DEDICATED target dir, so this runs while the app is running — which is the normal state, and
    # the state verify-10 sweeps in. build.rs copies onnxruntime.dll into the target dir, and the
    # running app holds a lock on the copy in target/release: a shared dir fails with
    # "The process cannot access the file because it is being used by another process. (os error 32)"
    # and the gate would be red for a reason that has nothing to do with performance. Costs one extra
    # compile the first time and is cached after; target/ is already gitignored.
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", str(APP / "src-tauri" / "target" / "bench"))
    proc = subprocess.run(cmd, capture_output=True, text=True, cwd=APP, env=env)
    out = proc.stdout + proc.stderr
    results: dict[str, int] = {}
    for line in out.splitlines():
        m = BENCH_LINE.match(line.strip())
        if m:
            results[m.group("name")] = int(m.group("ns").replace(",", ""))
    if proc.returncode != 0 and not results:
        print(out[-4000:], file=sys.stderr)
        raise SystemExit(f"cargo bench failed (exit {proc.returncode}) and produced no measurements")
    return results


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--update", action="store_true", help="rewrite the committed baseline from this run")
    ap.add_argument("--runs", type=int, default=1, help="repeat N times; use the fastest per bench")
    args = ap.parse_args()

    runs: list[dict[str, int]] = []
    for i in range(max(1, args.runs)):
        runs.append(measure())
        print(f"  run {i + 1}: {len(runs[-1])} benchmarks", flush=True)

    names = sorted(set().union(*[set(r) for r in runs]))
    if not names:
        # THE anti-vacuity guard: no measurements is a broken run, never a clean bill.
        print("BENCH GATE: FAIL - parsed ZERO benchmarks; refusing to report a pass", flush=True)
        return 1

    # FASTEST across runs, not the mean: a bench is a lower-bound measurement and the slow tail is
    # scheduler noise on a desktop that is also running an app. The spread is reported, not averaged
    # away, so the number below the threshold is honest about how quiet the machine was.
    best = {n: min(r[n] for r in runs if n in r) for n in names}
    spread = {
        n: (max(r[n] for r in runs if n in r) - min(r[n] for r in runs if n in r)) / min(r[n] for r in runs if n in r)
        for n in names
        if len(runs) > 1
    }

    if args.update:
        BASELINE.parent.mkdir(parents=True, exist_ok=True)
        BASELINE.write_text(
            json.dumps(
                {
                    "_comment": "Committed criterion baseline, ns/iter, FASTEST of the recorded runs. "
                    "Regenerate with `python scripts/bench_gate.py --update --runs 3` WHILE THE APP IS "
                    "RUNNING - that is the condition verify-10 sweeps in, and a baseline taken with it "
                    "stopped reported five false regressions of 1.16x-1.41x on the first real run. Do not "
                    "regenerate while something else is compiling either: that bakes a different machine "
                    "into the numbers just as surely.",
                    "budget": BUDGET,
                    "runs": len(runs),
                    "observed_spread": {n: round(v, 4) for n, v in sorted(spread.items())},
                    "benchmarks": {n: best[n] for n in names},
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"BENCH BASELINE WRITTEN: {len(names)} benchmarks -> {BASELINE.relative_to(APP)}")
        return 0

    if not BASELINE.exists():
        print(f"BENCH GATE: FAIL - no committed baseline at {BASELINE.relative_to(APP)}; run --update", flush=True)
        return 1
    base = json.loads(BASELINE.read_text(encoding="utf-8"))
    ref: dict[str, int] = base["benchmarks"]

    # A bench that vanished is a REGRESSION IN COVERAGE: silently dropping it would let a rename or a
    # deleted target read as "no regressions".
    missing = sorted(set(ref) - set(best))
    # The spread recorded WITH the baseline, so the allowance is the noise measured on the machine the
    # baseline came from — not whatever this run happened to see.
    base_spread: dict[str, float] = base.get("observed_spread", {})
    regressions = []
    unenforceable = []
    for n in sorted(set(ref) & set(best)):
        ratio = best[n] / ref[n]
        limit = threshold_for(n, base_spread)
        if limit is None:
            unenforceable.append((n, base_spread.get(n, 0.0), ratio))
            print(f"  {n:<44} {best[n]:>10,} ns  vs {ref[n]:>10,}  ({ratio:5.2f}x) NOT ENFORCED "
                  f"- {base_spread.get(n, 0.0):.0%} run-to-run noise on this machine")
            continue
        flag = "REGRESSION" if ratio > limit else "ok"
        print(f"  {n:<44} {best[n]:>10,} ns  vs {ref[n]:>10,}  ({ratio:5.2f}x, limit {limit:4.2f}x) {flag}")
        if ratio > limit:
            regressions.append((n, ref[n], best[n], ratio, limit))

    for n in sorted(set(best) - set(ref)):
        print(f"  {n:<44} {best[n]:>10,} ns  (NEW - not in baseline, not gated until --update)")

    if missing:
        print("BENCH GATE: FAIL - benchmarks in the baseline did not run:", flush=True)
        for n in missing:
            print(f"  - {n}")
        return 1
    if regressions:
        print(f"BENCH GATE: FAIL - {len(regressions)} bench(es) beyond their budget", flush=True)
        for n, b, now, ratio, limit in regressions:
            print(f"  - {n}: {b:,} -> {now:,} ns ({ratio:.2f}x, limit {limit:.2f}x)")
        return 1
    limits = {n: threshold_for(n, base_spread) for n in ref}
    tight = sum(1 for v in limits.values() if v is not None and v <= 1 + BUDGET + 1e-9)
    gated = sum(1 for v in limits.values() if v is not None)
    print(
        f"BENCH GATE: OK - {gated}/{len(ref)} benchmarks gated and within budget "
        f"({tight} at the charter's {BUDGET:.0%}, the rest at 2x their measured noise; "
        f"{len(unenforceable)} too noisy on this machine to gate and named above)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
