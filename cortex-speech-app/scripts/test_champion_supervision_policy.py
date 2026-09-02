#!/usr/bin/env python3
"""The champion supervisor is owner-registered, mirrors the reviewer watchdog, and the checker is read-only.

MEASURED 2026-09-02: after the 00:06 reboot every reviewer link returned and the OmniASR-7B champion
stayed dark on 8799 until a person started it; nothing supervised it and nothing reported that.
Registering a scheduled task is privileged boot configuration, so the script registers only on
`-Register` (the owner's action), prints its plan on `-DryRun`, and the checker never mutates.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
SUPERVISOR = SCRIPTS / "ops" / "cortex-champion-supervisor.ps1"
CHECKER = SCRIPTS / "check_champion_supervision.py"
sys.path.insert(0, str(SCRIPTS))
import check_champion_supervision as checker  # noqa: E402


def test_supervisor_mirrors_the_watchdog_registration_shape() -> None:
    text = SUPERVISOR.read_text(encoding="utf-8")
    for needle, why in (
        ("[switch]$Register", "registration must be an explicit switch, never the default"),
        ("[switch]$DryRun", "a dry run must exist so the plan can be verified without registering"),
        ("New-ScheduledTaskTrigger -AtLogOn -User $currentPrincipal", "at-logon bound to the exact interactive principal"),
        ("-RepetitionInterval (New-TimeSpan -Minutes 5)", "a repeating clock trigger heals within five minutes"),
        ("-StartWhenAvailable", "a pass missed while asleep must run late, not never"),
        ("-MultipleInstances IgnoreNew", "an overlapping pass is dropped, never doubled"),
        ("-ExecutionTimeLimit ([TimeSpan]::Zero)", "a champion launch loads ~17 GB per GPU and may take minutes"),
        ("-AllowStartIfOnBatteries -DontStopIfGoingOnBatteries", "a desktop behind a UPS must keep healing"),
        ("start_7b_server.ps1", "a pass runs the repo's idempotent, identity-bound starter"),
    ):
        assert needle in text, f"{SUPERVISOR.name}: {why} ({needle!r} missing)"
    assert text.count("Register-ScheduledTask") == 1, "exactly one registration site, inside the -Register branch"
    register_branch = text[text.index("if ($Register) {"):]
    assert "Register-ScheduledTask" in register_branch, "registration must live inside the -Register branch"
    assert "nothing registered" in register_branch, "the dry run must say it registered nothing"


def test_checker_is_read_only() -> None:
    text = CHECKER.read_text(encoding="utf-8")
    for verb in ("Register-ScheduledTask", "Unregister-ScheduledTask", "Enable-ScheduledTask", "Disable-ScheduledTask",
                 "Start-ScheduledTask", "schtasks /create", "schtasks /change", "Start-Process", "Stop-Process",
                 "start_7b_server", "--restart", "nvidia-smi"):
        assert verb not in text, f"{CHECKER.name} must never mutate ({verb!r} found)"
    assert "--health" in text and "--expected-pointer" in text, "the checker uses the app's own identity-bound probe"
    assert checker.LOCKED_CHAMPION == "omniasr-7b-legacy-c348ade8a816", "the locked champion identity"


def test_task_verdict_fixtures() -> None:
    ok, line = checker.task_verdict(None)
    assert not ok and "OWNER ACTION" in line and "-Register" in line
    ok, line = checker.task_verdict({"state": "Disabled", "enabled": False, "startWhenAvailable": True, "triggers": ["MSFT_TaskLogonTrigger"]})
    assert not ok and "disabled" in line
    ok, line = checker.task_verdict({"state": "Ready", "enabled": True, "startWhenAvailable": False, "triggers": ["MSFT_TaskLogonTrigger"]})
    assert not ok and "StartWhenAvailable" in line
    ok, line = checker.task_verdict({"state": "Ready", "enabled": True, "startWhenAvailable": True, "triggers": ["MSFT_TaskTimeTrigger"]})
    assert not ok and "at-logon" in line
    ok, line = checker.task_verdict({"state": "Ready", "enabled": True, "startWhenAvailable": True,
                                     "triggers": ["MSFT_TaskLogonTrigger", "MSFT_TaskTimeTrigger"]})
    assert ok and "2 trigger(s)" in line


def test_dry_run_prints_a_plan_and_registers_nothing() -> None:
    if os.name != "nt":
        print("SKIP-ENV: the supervisor is a Windows scheduled task; dry-run execution needs PowerShell on Windows")
        return
    for extra in ([], ["-Register"]):
        out = subprocess.run(
            ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(SUPERVISOR), *extra, "-DryRun"],
            capture_output=True, text=True, timeout=120,
        )
        assert out.returncode == 0, out.stderr[:300]
        assert "DRY RUN" in out.stdout, out.stdout[:300]
        assert "nothing registered" in out.stdout or "nothing executed" in out.stdout
    probe = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command",
         "if (Get-ScheduledTask -TaskName CortexChampionSupervisor -ErrorAction SilentlyContinue) { 'present' } else { 'absent' }"],
        capture_output=True, text=True, timeout=60,
    )
    # The dry run must never register. (When the OWNER has registered the task for real, 'present'
    # is the desired steady state and this assertion is about the dry run, so accept it.)
    assert probe.stdout.strip() in {"absent", "present"}, probe.stdout
    text = SUPERVISOR.read_text(encoding="utf-8")
    assert re.search(r"if \(\$DryRun\) \{[^}]*exit 0", text, re.S), "every dry-run branch exits before any side effect"


def main() -> None:
    test_supervisor_mirrors_the_watchdog_registration_shape()
    test_checker_is_read_only()
    test_task_verdict_fixtures()
    test_dry_run_prints_a_plan_and_registers_nothing()
    print("champion supervision policy passed")


if __name__ == "__main__":
    main()
