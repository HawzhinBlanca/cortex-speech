#!/usr/bin/env python3
"""Is the OmniASR-7B champion supervised across a reboot, and is it serving the locked identity?

Read-only. Two questions, answered separately so the report never conflates them:

1. Is the scheduled task `CortexChampionSupervisor` registered, enabled and set to start when
   available? (Registering it is an OWNER action: `scripts/ops/cortex-champion-supervisor.ps1
   -Register`. This script never registers, enables, starts or stops anything.)
2. Does the champion answer the app's own identity-bound health probe with the exact registry
   pointer (`cortex_7b_client.py --health --expected-pointer`)?

Exit 0 when both hold; 2 when the task is missing or disabled (an owner action is printed as such);
1 when the champion does not answer. Measured 2026-09-02: after a reboot every reviewer link came
back and the champion stayed dark until started by hand — the second question was green only because
a person had acted; the first was red and nothing said so.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
TASK_NAME = "CortexChampionSupervisor"
CLIENT = APP / "scripts" / "cortex_7b_client.py"
LOCKED_CHAMPION = "omniasr-7b-legacy-c348ade8a816"


def wsl_path(path: Path) -> str:
    """A Windows path as WSL sees it (drive letter mount); derived, never a literal profile path."""
    if not path.drive:
        return path.as_posix()
    return "/mnt/" + path.drive.rstrip(":").lower() + path.as_posix()[len(path.drive):]


DEFAULT_POINTER = wsl_path(Path(os.environ.get("APPDATA", "")) / "cortex-speech" / "champion.json")


def query_task(task_name: str = TASK_NAME) -> dict[str, object] | None:
    """The task's state as PowerShell reports it, or None when it is not registered. Read-only."""
    script = (
        "$t = Get-ScheduledTask -TaskName '" + task_name + "' -ErrorAction SilentlyContinue; "
        "if (-not $t) { exit 3 }; "
        "[pscustomobject]@{ state = [string]$t.State; enabled = [bool]$t.Settings.Enabled; "
        "startWhenAvailable = [bool]$t.Settings.StartWhenAvailable; "
        "triggers = @($t.Triggers | ForEach-Object { $_.CimClass.CimClassName }) } | ConvertTo-Json -Compress"
    )
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", script], capture_output=True, text=True, timeout=60
    )
    if out.returncode == 3:
        return None
    if out.returncode != 0:
        raise RuntimeError(f"task query failed ({out.returncode}): {out.stderr.strip()[:300]}")
    return json.loads(out.stdout)


def task_verdict(task: dict[str, object] | None) -> tuple[bool, str]:
    """Pure: the verdict on a task record. Fixture-tested."""
    if task is None:
        return False, f"OWNER ACTION: {TASK_NAME} is not registered; run scripts/ops/cortex-champion-supervisor.ps1 -Register"
    if not task.get("enabled"):
        return False, f"OWNER ACTION: {TASK_NAME} is registered but disabled (state {task.get('state')}); re-run scripts/ops/cortex-champion-supervisor.ps1 -Register (it re-registers with -Force, enabled)"
    if not task.get("startWhenAvailable"):
        return False, f"{TASK_NAME} lacks StartWhenAvailable: a pass missed while the machine slept is never run late"
    triggers = list(task.get("triggers") or [])
    if not any("Logon" in str(t) for t in triggers):
        return False, f"{TASK_NAME} has no at-logon trigger; it will not start the champion after a reboot"
    return True, f"{TASK_NAME} registered, enabled, at-logon + {len(triggers)} trigger(s), starts when available"


def champion_health(pointer: str) -> tuple[bool, str]:
    """The app's own identity-bound probe, through WSL. Never reads the DB, never touches the model."""
    python = os.environ.get("CORTEX_7B_WSL_PYTHON", "/home/ai/.venv-wsl-whisper/bin/python")
    client = wsl_path(CLIENT)
    out = subprocess.run(
        ["wsl", "--", python, client, "--health", "--expected-pointer", pointer],
        capture_output=True, text=True, timeout=120,
    )
    line = next((l for l in out.stdout.splitlines() if l.startswith("__HEALTH__=")), "")
    if out.returncode != 0 or not line:
        return False, f"champion does not answer the identity-bound probe (exit {out.returncode}): {(out.stderr or out.stdout).strip()[:200]}"
    payload = json.loads(line[len("__HEALTH__="):])
    if payload.get("status") != "ready" or payload.get("modelVersionId") != LOCKED_CHAMPION:
        return False, f"champion answers but is not the locked identity: status={payload.get('status')} model={payload.get('modelVersionId')}"
    return True, f"champion ready, {LOCKED_CHAMPION}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--pointer", default=DEFAULT_POINTER,
                        help="WSL path of the app's champion registry pointer")
    parser.add_argument("--skip-health", action="store_true", help="report the task state only")
    args = parser.parse_args()

    task_ok, task_line = task_verdict(query_task())
    print(("  ok  " if task_ok else "  RED ") + task_line)
    health_ok, health_line = (True, "health probe skipped") if args.skip_health else champion_health(args.pointer)
    print(("  ok  " if health_ok else "  RED ") + health_line)
    if not task_ok:
        print("CHAMPION SUPERVISION: OWNER ACTION REQUIRED (the champion may be serving now, but nothing brings it back after a reboot)")
        return 2
    if not health_ok:
        print("CHAMPION SUPERVISION: RED (supervised, but the champion is not serving the locked identity)")
        return 1
    print("CHAMPION SUPERVISION: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
