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

MEASURE THE SAME WAY ON BOTH SIDES. The baseline is the FASTEST of N runs, so the comparison takes the
fastest of N too — see `_resolve_runs`. Sampling once against a best-of-five baseline compares sampling,
not code, and biased every ratio upward.

Usage:
  python scripts/bench_gate.py            # compare against the baseline, matching its run count
  python scripts/bench_gate.py --update   # re-measure and REWRITE the baseline (deliberate act)
  python scripts/bench_gate.py --runs 3   # override the repeat count (calibration)
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

# The charter's number, kept here as the STANDARD — not as anything this machine can enforce.
#
# Measured 2026-08-02 on the reference machine, APP RUNNING (the condition a verify-10 sweep is in —
# see the docstring for why that matters), 5 runs of 12 benches: spread min 4.2%, median 9.2%,
# max 34.5%. The same 12 benches over only 3 runs read median 5.6%, and over 3 runs with the app
# STOPPED read median 5.8% / max 25.9% — three numbers for one machine, which is the whole lesson:
# the spread you measure depends on how many samples you take and what else is running, and the
# smaller, quieter measurement is the flattering one.
#
# A flat 5% budget is inside that noise for every bench here, so it would fire on a healthy machine
# most sweeps. A gate that cries wolf gets muted, which is worse than no gate. The thresholds below
# are therefore derived per-bench from measured noise, and the gate PRINTS the tightest one it is
# actually enforcing on every run — so this comment can never quietly drift away from the truth.
BUDGET = 0.05
# 3x, not 2x, and a 10% floor — both bought with a failure rather than reasoned in advance.
#
# The first version used 2x the spread from a 3-run calibration and produced a 1.088x limit for
# diff/identical_100_words. That bench then measured 1.0897x inside a sweep and FAILED, minutes after
# measuring 1.09x standalone and passing. Nothing had changed but which other legs had just run. A
# limit that a healthy machine lands on either side of is a coin flip, not a budget.
#
# The error was statistical: max-minus-min over THREE samples is a low estimate of a distribution's
# spread, and multiplying a low estimate by two keeps it low. More samples (calibrate with --runs 5)
# and a wider multiplier both push against that, and MIN_THRESHOLD stops the arithmetic ever producing
# a limit tighter than this machine can actually resolve.
NOISE_MULTIPLIER = 3.0

# No bench is gated tighter than this, whatever the arithmetic says. Which means: the charter's 5% is
# NOT enforced on any bench here, and pretending otherwise is what the previous version did. On this
# hardware, with the app running as it is during a sweep, 5% is inside the noise for every one of the
# twelve. The number is kept as BUDGET above so the gap between what the charter asks and what this
# machine can measure stays visible instead of being quietly redefined.
MIN_THRESHOLD = 1.10

# Past this, a "threshold" stops being a budget. On the 5-run app-up calibration, 10 of 12 benches
# derive a limit of 1.49x or tighter; TWO do not — audio/waveform_decode_16000_samples (the shortest,
# ~67us, where scheduler jitter dominates) and diff/identical_1000_words. Handing those a 2.04x or
# 1.67x limit would let a genuine 2x regression through while LOOKING gated, which is the exact
# dishonesty this file exists to avoid. They are reported as NOT ENFORCED, by name and with their
# noise figure, on every single run.
MAX_THRESHOLD = 1.50


def threshold_for(name: str, spread: dict[str, float]) -> float | None:
    """Allowed ratio before `name` counts as a regression, or None when the noise makes it unenforceable."""
    limit = max(MIN_THRESHOLD, 1.0 + NOISE_MULTIPLIER * spread.get(name, 0.0))
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


def _resolve_runs(explicit: int | None, updating: bool) -> int:
    """How many times to measure — matching the BASELINE's own run count unless told otherwise.

    THE BUG THIS FIXES, and it was mine. The baseline stores the FASTEST of N runs (5, as committed),
    and the gate sampled ONCE. Best-of-1 against best-of-5 is not a comparison of code; it is a
    comparison of sampling. A single run is slower than the best of five essentially always, so every
    ratio is biased upward by construction — and that is exactly what the numbers looked like: on two
    consecutive sweeps, ALL TWELVE benches read above baseline and not one read below. The failing set
    also moved between runs (audio/waveform_decode_1600000_samples: 1.91x, then 1.08x on the same
    commit), which is the other tell.

    Matching the counts makes the ratio mean what it claims. Note what this deliberately does NOT do:
    the budget and the per-bench thresholds are untouched. This removes a sampling bias; it does not
    widen a limit. A gate that reds on its own measurement method teaches people to ignore it.

    Read from the baseline rather than hardcoded, so regenerating with a different `--runs` keeps the
    two sides matched automatically instead of drifting apart again.
    """
    if explicit is not None:
        return max(1, explicit)
    if updating:
        return 1  # calibrating a fresh baseline: the caller says how many, one is the honest default
    try:
        recorded = int(json.loads(BASELINE.read_text(encoding="utf-8")).get("runs", 1))
    except (OSError, ValueError, TypeError):
        return 1  # no readable baseline: the run below fails loudly on that, not here
    return max(1, recorded)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--update", action="store_true", help="rewrite the committed baseline from this run")
    ap.add_argument(
        "--runs",
        type=int,
        default=None,
        help="repeat N times; use the fastest per bench. Default: MATCH the baseline's own run count "
        "(see _resolve_runs) — an explicit value is only for calibrating a new baseline.",
    )
    args = ap.parse_args()

    runs_wanted = _resolve_runs(args.runs, args.update)
    runs: list[dict[str, int]] = []
    for i in range(runs_wanted):
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
    gated = sum(1 for v in limits.values() if v is not None)
    tightest = min((v for v in limits.values() if v is not None), default=float("nan"))
    print(
        f"BENCH GATE: OK - {gated}/{len(ref)} benchmarks gated and within budget "
        f"(tightest limit {tightest:.2f}x; the charter's {BUDGET:.0%} is inside this machine's noise "
        f"for ALL of them, so none is gated at it; {len(unenforceable)} too noisy to gate at all, "
        f"named above)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
