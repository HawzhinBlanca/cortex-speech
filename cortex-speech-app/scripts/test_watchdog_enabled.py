#!/usr/bin/env python3
"""Fail if the availability watchdog is REGISTERED BUT SWITCHED OFF.

WHY THIS EXISTS. `CortexWatchdog` is the entire availability story for the review server: a scheduled
task that probes port 8737 every 5 minutes and relaunches the app when the server is unreachable. The
owner reviews clips on a phone against that server. If the task is off, a crash or a wedge is
permanent and silent — the link simply stops answering and nothing brings it back.

Turning it off is a documented, necessary step. `cortex-watchdog.ps1` says so in its own header:

    To stop the app ON PURPOSE without it resurrecting within 5 minutes:
      schtasks /change /tn CortexWatchdog /disable   (re-enable with /enable)

and `docs/REMOTE_PUBLIC_LINKS_PLAN.md` documents the same procedure. What neither documents is anything
that NOTICES the re-enable never happened. On 2026-08-03 this agent disabled the task to relink the exe
while the owner was reviewing, and the only thing that restored it was remembering to. A crash, an
interrupted session, or a sweep that died in between would have left the owner's phone link with no
healer and no warning — discovered whenever it next failed, which is exactly when it matters most.

WHAT IT CHECKS. Only the one state that is unambiguously wrong: the task EXISTS and is Disabled. That
is a machine someone meant to protect and left unprotected.

WHAT IT DELIBERATELY DOES NOT DO. It does not fail when the task is absent. A clean checkout, CI, a
non-Windows box and a machine that never opted into the availability stack all legitimately have no
watchdog, and reddening a gate on them would train people to ignore it. Absent is reported and skipped;
present-but-off is a failure. A guard that cannot tell those two apart is worse than no guard.
"""

from __future__ import annotations

import shutil
import subprocess
import sys

TASK = "CortexWatchdog"
REENABLE = f"schtasks /change /tn {TASK} /enable"


def skip(reason: str) -> int:
    print(f"SKIP: {reason}")
    return 0


def task_state() -> str | None:
    """The task's State, or None when it is not registered / cannot be queried.

    `Get-ScheduledTask` is preferred over parsing `schtasks /query` because its State is a CIM enum
    name (Ready/Running/Disabled) rather than display text, which `schtasks` localizes — a check that
    silently stops matching on a non-English Windows would report healthy forever.
    """
    ps = shutil.which("powershell") or shutil.which("pwsh")
    if not ps:
        return None
    r = subprocess.run(
        [ps, "-NoProfile", "-Command", f"(Get-ScheduledTask -TaskName '{TASK}' -ErrorAction SilentlyContinue).State"],
        capture_output=True,
        text=True,
        errors="replace",
    )
    state = r.stdout.strip()
    return state or None


def main() -> int:
    if sys.platform != "win32":
        return skip(f"{sys.platform} is not Windows; {TASK} is a Task Scheduler entry")

    state = task_state()
    if state is None:
        return skip(f"{TASK} is not registered on this machine (clean checkout / CI / availability stack not set up)")

    if state.lower() == "disabled":
        print(
            # ASCII only in the printed message: the Windows console renders an em-dash as a
            # replacement character, and the one line someone reads at 3am must not be mojibake.
            f"\nWATCHDOG GATE: FAIL - {TASK} is registered but DISABLED.\n"
            f"  The review server has no healer: if the app crashes or wedges, the phone link stays\n"
            f"  dead until somebody notices by hand. Disabling it is a legitimate step (relinking the\n"
            f"  exe, stopping the app on purpose) - leaving it disabled is not.\n"
            f"  Re-enable with:  {REENABLE}"
        )
        return 1

    print(f"WATCHDOG GATE: OK ({TASK} state={state})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
