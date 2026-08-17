#!/usr/bin/env python3
"""Gate D: the challenger loop must be wired, and any run it produced must be honest.

Phase 5 of docs/PLAN_TRUE_10.md. The loop is snapshot -> train -> eval -> verdict, and its danger is
not that a challenger loses — a REJECT is a perfectly good outcome and PASSES this gate. The danger
is a run that LOOKS finished:

  * a run record saying "trained" for training that never happened;
  * a verdict with no snapshot behind it, so nothing can say what was learned from what;
  * a PROMOTE verdict whose own numbers do not support it.

So this checks the chain is present, and audits every run record and verdict on disk for internal
consistency. Wiring alone is not a pass for the loop as a whole — `verify_10` reports SKIP-ENV until
a canary has actually run, which is stated rather than hidden.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
RUNS_DIR = REPO_ROOT.parent / "runs"

# The chain, in order. A missing link means the loop cannot complete without a human improvising,
# which is exactly what "zero manual steps" rules out.
REQUIRED_SCRIPTS = [
    ("train_challenger.py", "snapshot -> verified pack -> run record"),
    ("build_eval_slices.py", "the protected groups the verdict is checked against"),
    ("promotion_gate.py", "the verdict itself"),
]


def audit_run(record: dict) -> list[str]:
    """Ways a challenger run record could misrepresent what happened."""
    problems: list[str] = []
    status = record.get("status")
    if status not in {"prepared", "trained", "failed"}:
        problems.append(f"unknown status {status!r} — a run must say plainly what it did")
    if not record.get("snapshot_id"):
        problems.append("no snapshot_id — nothing can say what this run learned from")
    if status == "trained":
        if not record.get("trainer_command"):
            problems.append("status 'trained' with no trainer_command — training that cannot have happened")
        if record.get("trainer_exit_code") not in (0, None):
            problems.append(
                f"status 'trained' with trainer exit {record.get('trainer_exit_code')} — a failed trainer "
                "must be recorded as failed"
            )
    if status == "prepared" and record.get("trainer_exit_code") == 0:
        problems.append("status 'prepared' but the trainer reported success — one of the two is wrong")
    return problems


def audit_verdict(verdict: dict) -> list[str]:
    """Ways a promotion verdict could claim more than its own numbers support."""
    problems: list[str] = []
    decision = verdict.get("verdict")
    if decision not in {"PROMOTE", "REJECT", "INVALID"}:
        problems.append(f"unknown verdict {decision!r}")
        return problems
    if decision != "PROMOTE":
        return problems  # a REJECT needs no defending; that is the safe direction

    champion, challenger = verdict.get("champion_cer"), verdict.get("challenger_cer")
    if champion is None or challenger is None:
        problems.append("PROMOTE with no measured CER on one side")
    elif challenger >= champion:
        problems.append(
            f"PROMOTE while the challenger is not better ({champion} -> {challenger}) — the verdict "
            "contradicts its own numbers"
        )
    if not verdict.get("paired_clips"):
        problems.append("PROMOTE with no paired clips — nothing was compared")
    for entry in verdict.get("slices", []):
        before, after = entry.get("champion_cer"), entry.get("challenger_cer")
        if before is not None and after is not None and after > before:
            problems.append(
                f"PROMOTE while slice {entry.get('slice')!r} regressed ({before} -> {after})"
            )
    return problems


def main() -> int:
    scripts_dir = REPO_ROOT / "scripts"
    missing = [(name, why) for name, why in REQUIRED_SCRIPTS if not (scripts_dir / name).is_file()]
    if missing:
        print("CHALLENGER LOOP: FAIL", flush=True)
        for name, why in missing:
            print(f"  - scripts/{name} is missing — {why}", flush=True)
        return 1

    runs = sorted(RUNS_DIR.glob("challenger_*/challenger_run.json")) if RUNS_DIR.is_dir() else []
    verdicts = sorted(RUNS_DIR.glob("**/promotion_verdict.json")) if RUNS_DIR.is_dir() else []

    problems: list[str] = []
    trained = 0
    for path in runs:
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            problems.append(f"{path.parent.name}/challenger_run.json is unreadable: {error}")
            continue
        problems += [f"{path.parent.name}: {p}" for p in audit_run(record)]
        if record.get("status") == "trained":
            trained += 1
    for path in verdicts:
        try:
            verdict = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            problems.append(f"{path.parent.name}/promotion_verdict.json is unreadable: {error}")
            continue
        problems += [f"{path.parent.name}: {p}" for p in audit_verdict(verdict)]

    if problems:
        print("CHALLENGER LOOP: FAIL", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 1

    if not runs:
        # Honest: the chain is wired but nothing has run it. Wiring is not evidence, so this reports
        # SKIP-ENV rather than OK — a gate that says OK for an unrun loop is the flattering kind.
        print("CHALLENGER LOOP: SKIP-ENV (chain wired, no canary run yet — run train_challenger.py "
              "against a sealed snapshot)", flush=True)
        return 0

    print(f"CHALLENGER LOOP: OK ({len(runs)} run(s), {trained} trained, {len(verdicts)} verdict(s), "
          f"all internally consistent)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
