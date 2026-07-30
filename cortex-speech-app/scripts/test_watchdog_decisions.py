"""Drill every branch of cortex-watchdog.ps1's decision, without touching the real profile.

The watchdog is the whole availability story — it is the only thing that brings the review server back
after a crash, a wedge or a reboot — and it had no test of any kind. Its most dangerous branch
force-kills the app, and the branch that must LEAVE A HEALTHY APP ALONE (the owner pressed Stop) was
reviewed but never verified. It could not be verified for real: proving it means pressing Stop, which
deletes couch_session.json and revokes the owner's live link.

So the script grew a `-DryRun` that decides and REPORTS ("WATCHDOG-ACTION: ...") without killing or
launching, plus CORTEX_WATCHDOG_DATA_DIR / CORTEX_WATCHDOG_PORT overrides. This drill asserts the
CHOICE for each state, against a throwaway data dir and a port nothing is listening on, so it is safe
to run at any time — including while the owner is mid-review.

The process-alive states are only exercised when the real app happens to be running (the script matches
by exe PATH, which cannot be faked from here); they are reported as skipped rather than silently passed.

Usage: python scripts/test_watchdog_decisions.py
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
SCRIPT = APP / "scripts" / "ops" / "cortex-watchdog.ps1"
EXE = APP / "src-tauri" / "target" / "release" / "cortex-speech-app.exe"


def free_port() -> int:
    """A port nothing is listening on, so the probe is guaranteed to fail fast."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run(data_dir: Path, port: int) -> str:
    env = dict(os.environ, CORTEX_WATCHDOG_DATA_DIR=str(data_dir), CORTEX_WATCHDOG_PORT=str(port))
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(SCRIPT), "-DryRun"],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    lines = [ln for ln in (out.stdout or "").splitlines() if ln.startswith("WATCHDOG-ACTION:")]
    if not lines:
        raise AssertionError(f"no decision reported.\nstdout:\n{out.stdout}\nstderr:\n{out.stderr}")
    return lines[-1].split("WATCHDOG-ACTION:", 1)[1].strip()


def app_is_running() -> bool:
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command",
         "@(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue |"
         f" Where-Object {{ $_.Path -eq '{EXE}' }}).Count"],
        capture_output=True, text=True, timeout=120,
    )
    return (out.stdout or "0").strip() not in ("", "0")


def main() -> int:
    if not SCRIPT.exists():
        print(f"watchdog script missing: {SCRIPT}", file=sys.stderr)
        return 1
    dead = free_port()
    running = app_is_running()
    failures: list[str] = []
    checked = 0

    def expect(label: str, got: str, want: str) -> None:
        nonlocal checked
        checked += 1
        if want not in got:
            failures.append(f"{label}: expected {want!r}, got {got!r}")
        print(f"  {'OK  ' if want in got else 'FAIL'} {label}: {got}")

    tmp = Path(tempfile.mkdtemp(prefix="cortex-wd-"))
    try:
        print(f"drilling on a throwaway profile ({tmp}) and dead port {dead}")

        # NO session file. What happens depends on whether a process is alive, and that is the branch
        # that matters most: killing a healthy app after a deliberate Stop would make Stop feel haunted.
        if running:
            expect("no session + app running -> must NOT touch it", run(tmp, dead), "leave-alone")
        else:
            print("  SKIP no session + app running (real app is not running; cannot fake a PATH match)")

        # Session file present = the server is SUPPOSED to be up.
        (tmp / "couch_session.json").write_text('{"reviewers":{},"db_path":"x","spot_checks":[]}', encoding="utf-8")
        if running:
            expect("session + app running -> kill and relaunch", run(tmp, dead), "kill-and-relaunch")
            # THE KILL-LOOP CAP: three failed restarts and it must stop, not keep killing forever.
            (tmp / "logs").mkdir(exist_ok=True)
            (tmp / "logs" / "watchdog-kills.txt").write_text("3", encoding="utf-8")
            expect("session + 3 prior kills -> give up, stop killing", run(tmp, dead), "give-up")
            (tmp / "logs" / "watchdog-kills.txt").unlink()
        else:
            print("  SKIP session + app running (real app is not running)")
            expect("session + app NOT running -> relaunch", run(tmp, dead), "relaunch")

        # NOT drilled here: the live-port leg. An accept-only socket is exactly the wedged case the
        # 3x20s probe exists to wait out, so asserting it costs a minute of real sleep per run; the
        # owner's live server already proves the alive path on every real tick (last result 0).

        if failures:
            print("\nFAILURES:")
            for f in failures:
                print(f"  {f}")
            return 1
        print(f"\nwatchdog decision drill passed ({checked} branch assertion(s))")
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
