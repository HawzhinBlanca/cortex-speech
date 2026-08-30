#!/usr/bin/env python3
"""Measure how far this system is from bulletproof, and name the ONE next target.

Written for `docs/ROBUSTNESS_LOOP.md`. A fresh session with no memory runs this first and is handed
a specific function to work on -- not a front, not a theme, a function -- ranked from real
measurement. Everything here READS artifacts other tools produced; it measures nothing itself, so it
can never disagree with the gates.

Three fronts, in strict priority order:

  P0 live      reviewers can work RIGHT NOW (review-health.json heartbeat, written by the 30-min
               probe). A red here outranks everything: unreviewed clips are the product, and a
               reviewer who cannot get in is losing paid time this minute.
  P1 coverage  the five critical domains from rust_quality_gate.CRITICAL_COVERAGE_DOMAINS against
               their real thresholds. Ranked by UNCOVERED BRANCH COUNT, because that is the work
               list; a domain 3 points short of the bar with 400 uncovered branches is more work
               than one 30 points short with 12.
  P2 hygiene   unpushed commits and drift between the live ops scripts and their tracked copies.

STALENESS IS A FAILURE, NOT A FOOTNOTE. A coverage JSON older than the newest commit touching
src-tauri/src describes a tree that no longer exists; reporting it as current is exactly the
"claim a number you did not measure" failure this repo's honesty law forbids. This script refuses
to score coverage in that case and says re-measure instead.

  python scripts/robustness_scoreboard.py [--coverage <path>] [--json]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
REPO = APP.parent
DEFAULT_COVERAGE = APP / "logs" / "robustness" / "coverage-latest.json"
HEARTBEAT = APP / "logs" / "review-health.json"
HEARTBEAT_MAX_AGE = timedelta(minutes=75)  # two 30-min cycles plus slack

sys.path.insert(0, str(APP / "scripts"))
from rust_quality_gate import CRITICAL_COVERAGE_DOMAINS  # noqa: E402

# Mirrors verify_10.py's registry. Duplicated deliberately: if the gate's thresholds move, this
# scoreboard must NOT silently follow -- the pin below is a second witness.
OVERALL = {"lines": 85.0, "regions": 85.0, "functions": 80.0, "branches": 80.0}
CRITICAL = {"lines": 95.0, "regions": 95.0, "functions": 90.0, "branches": 90.0}


def run(cmd: list[str], cwd: Path) -> str:
    try:
        return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=120).stdout.strip()
    except Exception:
        return ""


def matches_domain(filename: str, patterns: tuple[str, ...]) -> bool:
    norm = filename.replace("\\", "/")
    for pattern in patterns:
        base = pattern.replace("\\", "/")
        if base.endswith("/*.rs"):
            if base[: -len("/*.rs")] in norm:
                return True
        elif norm.endswith(base):
            return True
    return False


def coverage_freshness(coverage: Path) -> tuple[bool, str]:
    """Is this report describing the tree that exists NOW, in the worktree it was measured in?

    First version compared the report's mtime against the MAIN tree's src commit time. That is the
    wrong tree: coverage is measured in an isolated worktree on its own branch, so a report taken
    before two commits on that branch still read as "fresh" and the scoreboard then named a
    function whose tests were already written. Identity, not timestamps: the sidecar records the
    exact commit measured, and the only question is whether that worktree still sits on it.
    """
    meta_path = coverage.with_suffix(".meta.json")
    if not meta_path.is_file():
        return False, f"UNVERIFIABLE: no {meta_path.name} recording which commit was measured -- re-measure"
    meta = json.loads(meta_path.read_text(encoding="utf-8-sig"))
    measured_sha = str(meta.get("measuredFromSha", ""))
    tree = Path(str(meta.get("measuredIn", "")))
    when = str(meta.get("measuredAtUtc", "unknown time"))
    if not measured_sha or not tree.is_dir():
        return False, f"UNVERIFIABLE: {meta_path.name} does not name a commit and an existing worktree -- re-measure"
    head = run(["git", "rev-parse", "HEAD"], tree)
    if head != measured_sha:
        return False, (
            f"STALE: measured {measured_sha[:8]} in {tree.name} at {when}, but that worktree is now on "
            f"{head[:8] or '(unreadable)'} -- re-measure before trusting any number"
        )
    return True, f"{measured_sha[:8]} in {tree.name}, measured {when}"


def live_front() -> tuple[str, list[str]]:
    if not HEARTBEAT.is_file():
        return "UNKNOWN", ["no review-health.json -- is the CortexReviewHealthProbe task running?"]
    # utf-8-SIG, not utf-8: the probe writes this with PowerShell 5.1 `Set-Content -Encoding utf8`,
    # which prepends a BOM that json.loads rejects outright. Reading it as utf-8 made this
    # scoreboard crash on a perfectly healthy heartbeat. utf-8-sig accepts both forms, so fixing
    # the reader is strictly safer than editing the probe that is currently supervising reviewers.
    payload = json.loads(HEARTBEAT.read_text(encoding="utf-8-sig"))
    at = datetime.fromisoformat(str(payload.get("at"))).astimezone(timezone.utc)
    age = datetime.now(timezone.utc) - at
    detail = str(payload.get("detail", ""))
    if age > HEARTBEAT_MAX_AGE:
        return "RED", [f"heartbeat is {int(age.total_seconds() // 60)} min old ({at:%Y-%m-%d %H:%M UTC}) -- probe stopped"]
    if not payload.get("ok"):
        return "RED", [line.strip() for line in detail.splitlines() if line.strip()][:6]
    return "GREEN", [f"all gates green {int(age.total_seconds() // 60)} min ago"]


def coverage_front(coverage: Path) -> tuple[dict, list[tuple]]:
    export = json.loads(coverage.read_text(encoding="utf-8"))["data"][0]
    totals = export["totals"]
    domains: dict[str, dict] = {}
    for name, patterns in CRITICAL_COVERAGE_DOMAINS.items():
        acc = {k: [0, 0] for k in ("lines", "branches", "regions", "functions")}
        for f in export["files"]:
            if not matches_domain(f["filename"], patterns):
                continue
            for k in acc:
                summary = f["summary"].get(k)
                if summary:
                    acc[k][0] += summary["covered"]
                    acc[k][1] += summary["count"]
        domains[name] = {
            k: {
                "percent": (100.0 * c / t) if t else 100.0,
                "uncovered": t - c,
                "target": CRITICAL[k],
            }
            for k, (c, t) in acc.items()
        }

    # Ranked worklist: functions with uncovered code inside a domain that is BELOW its branch bar.
    behind = {n for n, m in domains.items() if m["branches"]["percent"] < CRITICAL["branches"]}
    patterns = {p for n in behind for p in CRITICAL_COVERAGE_DOMAINS[n]}
    targets = []
    for func in export.get("functions", []):
        files = func.get("filenames", [])
        if not any(matches_domain(f, tuple(patterns)) for f in files):
            continue
        branches = func.get("branches", [])
        regions = func.get("regions", [])
        unc_br = sum(1 for b in branches if b[4] == 0) + sum(1 for b in branches if b[5] == 0)
        unc_rg = sum(1 for r in regions if r[4] == 0)
        if unc_br == 0:
            continue  # branch bars are the binding constraint; region-only gaps rank below
        lines = sorted({r[0] for r in regions if r[4] == 0})
        span = f"L{lines[0]}-{lines[-1]}" if lines else "L?"
        short = files[0].replace("\\", "/").split("/src/")[-1]
        targets.append((unc_br, unc_rg, short, span, func["name"][-70:]))
    targets.sort(reverse=True)
    return {"totals": totals, "domains": domains}, targets


def hygiene_front() -> tuple[str, list[str]]:
    notes: list[str] = []
    worktrees = [line.split()[1] for line in run(["git", "worktree", "list", "--porcelain"], REPO).splitlines() if line.startswith("worktree ")]
    for tree in worktrees:
        path = Path(tree)
        branch = run(["git", "branch", "--show-current"], path)
        if not branch:
            continue
        for remote in ("origin", "mirror"):
            ref = f"{remote}/{branch}"
            if run(["git", "rev-parse", "--verify", "-q", ref], path):
                ahead = run(["git", "rev-list", "--count", f"{ref}..HEAD"], path)
                if ahead.isdigit() and int(ahead) > 0:
                    notes.append(f"{path.name} [{branch}] has {ahead} unpushed commit(s) vs {remote}")
    live_ops = [
        ("scripts/check_reviewer_link_continuity.py", "cortex-deploy"),
        ("scripts/reviewer_link_vault.py", "cortex-deploy"),
        ("scripts/ops/review-health-probe.ps1", "cortex-deploy"),
    ]
    for rel, tracked_tree in live_ops:
        live = APP / rel
        tracked = REPO.parent / tracked_tree / "cortex-speech-app" / rel
        if live.is_file() and tracked.is_file() and live.read_bytes() != tracked.read_bytes():
            notes.append(f"DRIFT: live {rel} differs from its tracked copy in {tracked_tree}")
    return ("GREEN" if not notes else "RED"), (notes or ["no unpushed work, no ops drift"])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--coverage", default=str(DEFAULT_COVERAGE))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    live_status, live_notes = live_front()
    hyg_status, hyg_notes = hygiene_front()
    coverage = Path(args.coverage)

    print("ROBUSTNESS SCOREBOARD")
    print(f"\nP0 LIVE PIPELINE: {live_status}")
    for note in live_notes:
        print(f"     {note}")

    cov_payload, targets = None, []
    if not coverage.is_file():
        print(f"\nP1 COVERAGE: UNKNOWN -- no report at {coverage}")
        print("     run: cargo +<pinned> llvm-cov --locked --all-targets --all-features --branch --json --output-path <path>")
    else:
        fresh, freshness = coverage_freshness(coverage)
        if not fresh:
            print(f"\nP1 COVERAGE: UNKNOWN -- {freshness}")
        else:
            cov_payload, targets = coverage_front(coverage)
            totals = cov_payload["totals"]
            # deficit > 0 means BEHIND the bar. The first version took min() of (target - actual)
            # and called >= 0 green, which printed GREEN at lines=79.12/85 -- a scoreboard claiming
            # done while every bar was unmet. Green requires EVERY overall bar AND every critical
            # domain bar to be met; one unmet bar is a red gate, exactly as the CI gate treats it.
            deficit = max(OVERALL[k] - totals[k]["percent"] for k in OVERALL)
            domains_behind = [
                name
                for name, metrics in cov_payload["domains"].items()
                if any(metrics[k]["percent"] < metrics[k]["target"] for k in metrics)
            ]
            green = deficit <= 0 and not domains_behind
            print(f"\nP1 COVERAGE: {'GREEN' if green else 'RED'}  ({freshness})")
            print("     overall   " + "  ".join(
                f"{k}={totals[k]['percent']:.2f}/{OVERALL[k]:.0f}" for k in ("lines", "branches", "functions")
            ))
            for name, metrics in sorted(
                cov_payload["domains"].items(), key=lambda kv: kv[1]["branches"]["uncovered"], reverse=True
            ):
                br = metrics["branches"]
                flag = "OK " if br["percent"] >= br["target"] else "-- "
                print(
                    f"     {flag}{name:9} branches {br['percent']:6.2f}/{br['target']:.0f}"
                    f"  ({br['uncovered']} uncovered)   lines {metrics['lines']['percent']:6.2f}/{metrics['lines']['target']:.0f}"
                )

    print(f"\nP2 HYGIENE: {hyg_status}")
    for note in hyg_notes:
        print(f"     {note}")

    print("\nNEXT TARGET")
    if live_status == "RED":
        print("  P0: the live pipeline is red. Fix reviewer serving before any other work.")
        for note in live_notes:
            print(f"      {note}")
    elif targets:
        for unc_br, unc_rg, path, span, name in targets[:5]:
            print(f"  {unc_br:4} uncovered branches  {path} {span}  {name}")
        print("\n  Take the top row. Write refusal-arm tests for it, prove them, push, re-measure.")
    elif cov_payload:
        print("  Coverage bars are met. Run `make verify-10` and take the first red gate.")
    else:
        print("  Re-measure coverage first (see above), then re-run this scoreboard.")

    if args.json:
        print("\n" + json.dumps(
            {"live": live_status, "hygiene": hyg_status, "coverage": cov_payload, "targets": targets[:20]},
            indent=2,
        ))
    return 0


if __name__ == "__main__":
    sys.exit(main())
