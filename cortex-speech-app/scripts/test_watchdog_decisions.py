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

EVERY branch is drilled on every run, including the three that need a LIVE process. Those used to be
skipped whenever the real app was down — so the force-kill decision's coverage depended on machine
state, which is no coverage at all for the line most able to destroy a reviewer's in-flight work. The
script now also honours CORTEX_WATCHDOG_EXE, and this drill starts its own decoy process at a throwaway
path and points the override there. The owner's live app is EXCLUDED by the watchdog's own path filter,
and the last two assertions prove the decoy is what matched rather than the real app happening to
produce the same answer.

Usage: python scripts/test_watchdog_decisions.py
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
SCRIPT = APP / "scripts" / "ops" / "cortex-watchdog.ps1"
EXE = APP / "src-tauri" / "target" / "release" / "cortex-speech-app.exe"


def free_port() -> int:
    """A port nothing is listening on, so the probe is guaranteed to fail fast."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run(data_dir: Path, port: int, exe: Path | None = None, grace_min: str = "0") -> str:
    # grace_min="0" by default so a freshly spawned decoy process is already past the STARTUP GRACE
    # and the kill path is reachable. Without this every kill case would report "starting-up": the
    # decoy is seconds old and the real grace is 20 minutes. The grace itself is covered by its own
    # case below, which runs with the production default.
    env = dict(
        os.environ,
        CORTEX_WATCHDOG_DATA_DIR=str(data_dir),
        CORTEX_WATCHDOG_PORT=str(port),
        CORTEX_WATCHDOG_STARTUP_GRACE_MIN=grace_min,
    )
    if exe is not None:
        env["CORTEX_WATCHDOG_EXE"] = str(exe)
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


def start_decoy(dir_: Path) -> tuple[subprocess.Popen[bytes], Path]:
    """A live process the watchdog will match as "the app", without going near the real one.

    The three branches that turn on a running process used to be drilled only when the real app
    happened to be up — so the force-kill decision, the most dangerous line in the availability path,
    had coverage that depended on machine state. The script matches by process NAME and full PATH, and
    both are reproducible: copy any long-lived executable to `<tmp>\\cortex-speech-app.exe`, run it, and
    point CORTEX_WATCHDOG_EXE at that copy. The real app is then explicitly EXCLUDED by the same path
    filter, so this is safe to run mid-review — and -DryRun means nothing is killed either way.
    """
    dir_.mkdir(parents=True, exist_ok=True)
    exe = dir_ / "cortex-speech-app.exe"
    shutil.copy2(Path(os.environ["SystemRoot"]) / "System32" / "WindowsPowerShell" / "v1.0" / "powershell.exe", exe)
    proc = subprocess.Popen(
        [str(exe), "-NoProfile", "-Command", "Start-Sleep -Seconds 300"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
    )
    # Confirm the watchdog's own matcher can actually see it before asserting anything about branches
    # it is supposed to reach; a decoy that does not match would make every assertion below vacuous.
    for _ in range(50):
        out = subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command",
             "@(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue |"
             f" Where-Object {{ $_.Path -eq '{exe}' }}).Count"],
            capture_output=True, text=True, timeout=120,
        )
        if (out.stdout or "0").strip() not in ("", "0"):
            return proc, exe
        time.sleep(0.2)
    proc.kill()
    raise AssertionError(f"decoy process at {exe} never became visible to the watchdog's matcher")


def main() -> int:
    # The subject is a PowerShell script driven by a Windows Task Scheduler entry, and this drill
    # copies `System32\WindowsPowerShell\v1.0\powershell.exe` to build its decoy. None of that exists
    # on Linux or macOS, so the drill could only ever fail there — which it did, on every CI run,
    # taking `Linux Build Smoke` and `macOS Build Smoke` down with it.
    #
    # Skipping matches how `test_watchdog_enabled.py` already treats the same subject, and skipping is
    # honest here in a way it usually is not: the thing under test is ABSENT on this platform, not
    # unverified on it. The Windows sweep still drills every branch, which is the machine that has a
    # watchdog to protect.
    if sys.platform != "win32":
        print(f"SKIP: {sys.platform} is not Windows; cortex-watchdog.ps1 is a Task Scheduler entry")
        return 0

    if not SCRIPT.exists():
        print(f"watchdog script missing: {SCRIPT}", file=sys.stderr)
        return 1
    dead = free_port()
    # Reported as context, not used as a gate any more: coverage no longer depends on it.
    running = app_is_running()
    failures: list[str] = []
    checked = 0

    def expect(label: str, got: str, want: str) -> None:
        """Compare the ACTION TOKEN exactly, never as a substring.

        This started as `want in got` and that was a false pass generator: the reported actions include
        both `relaunch (session expected, not running)` and `kill-and-relaunch (attempt 1/3)`, and
        "relaunch" is a substring of "kill-and-relaunch" — so a drill expecting a plain relaunch
        happily went green on a decision to FORCE-KILL the app first. Caught by a fail-before run,
        where the line printed OK next to visibly wrong output. The action is always the first
        whitespace-delimited token; the parenthetical after it is detail.
        """
        nonlocal checked
        checked += 1
        action = got.split(None, 1)[0] if got else ""
        ok = action == want
        if not ok:
            failures.append(f"{label}: expected action {want!r}, got {action!r} (full: {got!r})")
        print(f"  {'OK  ' if ok else 'FAIL'} {label}: {got}")

    tmp = Path(tempfile.mkdtemp(prefix="cortex-wd-"))
    try:
        print(f"drilling on a throwaway profile ({tmp}) and dead port {dead}")
        print(f"  (the real app is {'running' if running else 'not running'} - every branch below is drilled either way)")

        # ── the process-DEAD branches, against a path nothing is running from ──
        # NO session file and nothing running: the autostart leg.
        no_proc_exe = tmp / "not-running" / "cortex-speech-app.exe"
        expect("no session + not running -> launch", run(tmp, dead, no_proc_exe), "launch")

        # Session file present = the server is SUPPOSED to be up, so a dead port means bring it back.
        (tmp / "couch_session.json").write_text('{"reviewers":{},"db_path":"x","spot_checks":[]}', encoding="utf-8")
        expect("session + not running -> relaunch", run(tmp, dead, no_proc_exe), "relaunch")

        # ── the process-ALIVE branches, against a decoy ────────────────────────
        # These are the ones that used to be SKIPped whenever the real app was down, which left the
        # force-kill decision covered only by luck. The decoy makes all three deterministic, and the
        # real app is excluded by the watchdog's own path filter, so this is safe mid-review.
        decoy, decoy_exe = start_decoy(tmp)
        try:
            # A running app with NO session file is the owner having pressed Stop. Killing it here
            # would resurrect-loop the app every five minutes and make Stop feel haunted.
            (tmp / "couch_session.json").unlink()
            expect("no session + RUNNING -> must NOT touch it", run(tmp, dead, decoy_exe), "leave-alone")

            # Session expected, process alive, port dead = wedged. Kill and relaunch.
            (tmp / "couch_session.json").write_text(
                '{"reviewers":{},"db_path":"x","spot_checks":[]}', encoding="utf-8"
            )
            expect("session + RUNNING + dead port -> kill and relaunch", run(tmp, dead, decoy_exe), "kill-and-relaunch")

            # THE KILL-LOOP CAP: three failed restarts and it must stop, not keep killing forever.
            (tmp / "logs").mkdir(exist_ok=True)
            (tmp / "logs" / "watchdog-kills.txt").write_text("3", encoding="utf-8")
            expect("session + 3 prior kills -> give up, stop killing", run(tmp, dead, decoy_exe), "give-up")
            (tmp / "logs" / "watchdog-kills.txt").unlink()

            # The decoy must still be alive: -DryRun decides and reports, it never kills. If this
            # fails, a "dry" run is destroying processes and the drill itself is dangerous to run.
            checked += 1
            if decoy.poll() is not None:
                failures.append("-DryRun killed the decoy process; a dry run must never kill anything")
            else:
                print("  OK   -DryRun reported three kill decisions without killing anything")
        finally:
            decoy.kill()
            decoy.wait(timeout=30)

        # THE DECOY MUST BE THE THING THAT MATCHED, not the owner's live app happening to produce the
        # same decision. Point the override at a path where nothing runs while the decoy is still up:
        # if the alive-branch results above came from the decoy, this must now report a DEAD process.
        # (Run last so the decoy is already gone — hence the fresh one below.)
        decoy2, decoy2_exe = start_decoy(tmp / "second")
        try:
            (tmp / "couch_session.json").write_text(
                '{"reviewers":{},"db_path":"x","spot_checks":[]}', encoding="utf-8"
            )
            expect("decoy alive but override points elsewhere -> DEAD", run(tmp, dead, no_proc_exe), "relaunch")
            expect("...and the same run against the decoy path -> ALIVE", run(tmp, dead, decoy2_exe), "kill-and-relaunch")

            # STARTUP GRACE (2026-08-15). The same inputs as the kill case above — session file
            # present, process alive, port dead — must NOT kill when the process is young. This is
            # the regression that took the app down: after the library tripled to 14,828 clips
            # startup needed longer than the 5-minute check interval, so the watchdog killed the app
            # three times running and it never once reached the point of serving. Run with the
            # PRODUCTION grace (no override) against a decoy that is seconds old.
            expect(
                "young process + dead port -> leave it alone to finish starting",
                run(tmp, dead, decoy2_exe, grace_min=""),
                "starting-up",
            )
        finally:
            decoy2.kill()
            decoy2.wait(timeout=30)

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
