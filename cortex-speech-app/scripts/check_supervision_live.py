#!/usr/bin/env python3
"""The can-this-machine-actually-serve-reviewers gate.

Every other gate in the sweep asks whether the CODE is correct. None of them asks whether the
running system is in a state where a paid reviewer opening their link gets anything at all. On
2026-08-15 all of these were true at once and the sweep would still have gone GREEN:

  * CortexWatchdog sat `Disabled` — turned off by the rebuild procedure hours earlier and never
    turned back on. It is the ONLY thing that restarts the app, so nothing was supervising.
  * The app had exited (orderly, `logs/last-exit.txt`), so port 8737 was dead while
    `couch_session.json` still listed five reviewers. Every link that had been sent was a dead link.
  * The C: drive was at 0 bytes free, which had already broken the 10-minute DB snapshot safety net
    ("periodic DB snapshot failed") — the one thing protecting irreplaceable review labour.

Each is invisible to a source-level gate and each silently destroys reviewer work or trust. Hence a
gate that looks at the machine, not the tree.

Exit 0 = supervised, reachable, and with room to write. Exit 1 = one or more of those is false.
The pure decision logic is `evaluate_supervision` and is unit-tested by `test_supervision_live.py`.
"""

from __future__ import annotations

import json
import hashlib
import os
import shutil
import ssl
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

LEGACY_WATCHDOG_TASK = "CortexWatchdog"
PRIVATE_WATCHDOG_TASK = "CortexPrivateProductionWatchdog"
WATCHDOG_TASK = LEGACY_WATCHDOG_TASK
COUCH_URL = "http://127.0.0.1:8737/"
# Same port, TLS — the couch server self-signs a certificate and serves HTTPS on every interface.
COUCH_URL_TLS = "https://127.0.0.1:8737/"
# 10 snapshots of an ~88 MB DB plus its WAL, the media cache and a release build's churn. Measured
# 2026-08-15: at 0 bytes the snapshot thread failed every cycle while the app reported healthy.
DEFAULT_FLOOR_GB = 20.0


def evaluate_supervision(
    *,
    watchdog_state: str | None,
    watchdog_starts_when_available: bool | None = None,
    session_expected: bool,
    reviewer_count: int,
    couch_status: int | None,
    free_bytes: int,
    floor_bytes: int,
) -> list[str]:
    """Pure decision core. Returns a list of problems; empty list means fit to serve."""
    problems: list[str] = []

    if watchdog_state is None:
        problems.append(
            f"{WATCHDOG_TASK} scheduled task is not registered — nothing will restart the app when it "
            f"exits, and it exits (5 times in the last week). Register it with scripts/ops/install-watchdog.ps1."
        )
    elif watchdog_state.strip().lower().startswith("disabled"):
        problems.append(
            f"{WATCHDOG_TASK} is DISABLED — the app is unsupervised. Re-enable with "
            f"`schtasks /change /tn {WATCHDOG_TASK} /enable`. (A rebuild disables it; something has to turn it back on.)"
        )
    elif watchdog_starts_when_available is False:
        # REGISTERED, ENABLED, AND STILL USELESS AFTER A SLEEP. Windows DROPS a repetition occurrence
        # that comes due while the machine is asleep or off unless StartWhenAvailable is set; it does
        # not run it late. cortex-watchdog.ps1 registers the task WITH -StartWhenAvailable, so a live
        # task without it has drifted from its own definition and nothing was watching for that.
        #
        # Measured 2026-08-18 from watchdog.log: 19 clean-exit relaunches, median ~8 min of downtime,
        # and one that took 9 h 18 m (exit 2026-08-13 19:23 UTC, detected 2026-08-14 07:42 local).
        # The couch server lives inside the app, so every one of those windows is every reviewer link
        # DEAD, not merely an import that fails.
        problems.append(
            f"{WATCHDOG_TASK} is registered and enabled but StartWhenAvailable is FALSE, so Windows "
            f"silently DROPS every check that comes due while the machine sleeps instead of running it "
            f"late — measured cost: one 9 h 18 m outage with every reviewer link dead. The live task has "
            f"drifted from scripts/ops/cortex-watchdog.ps1, which registers it WITH -StartWhenAvailable. "
            f"Re-register: powershell -ExecutionPolicy Bypass -File "
            f"cortex-speech-app/scripts/ops/cortex-watchdog.ps1 -Register"
        )

    if session_expected:
        if couch_status is None:
            problems.append(
                f"{reviewer_count} reviewer link(s) are live in couch_session.json but nothing answers on "
                f"{COUCH_URL} — every link already sent is DEAD. Start the app, or stop the session from Settings."
            )
        elif couch_status != 200:
            problems.append(
                f"couch review server answered HTTP {couch_status} (expected 200) with {reviewer_count} "
                f"reviewer link(s) live — reviewers cannot work."
            )

    if free_bytes < floor_bytes:
        problems.append(
            f"only {free_bytes / 2**30:.1f} GB free on the data drive (floor {floor_bytes / 2**30:.0f} GB). "
            f"The 10-minute DB snapshot safety net fails silently at low disk, and SQLite needs room for "
            f"its WAL — review work is at risk before anything reports an error."
        )

    return problems


def _watchdog_starts_when_available() -> bool | None:
    """The live task's StartWhenAvailable flag, or None when it cannot be read.

    Deliberately a SEPARATE probe from the State query: a task can be perfectly `Ready` and still be
    unable to heal anything after the machine wakes, which is the failure this arm exists to catch.
    """
    try:
        out = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                f"(Get-ScheduledTask -TaskName '{WATCHDOG_TASK}' -ErrorAction SilentlyContinue)"
                ".Settings.StartWhenAvailable",
            ],
            capture_output=True,
            text=True,
        )
    except (OSError, FileNotFoundError):
        return None
    if out.returncode != 0:
        return None
    answer = out.stdout.strip().lower()
    if answer in {"true", "false"}:
        return answer == "true"
    return None


def _watchdog_state() -> str | None:
    """The task's Status line, or None if the task is not registered / schtasks is unavailable."""
    try:
        out = subprocess.run(
            ["schtasks", "/query", "/tn", WATCHDOG_TASK, "/fo", "LIST"],
            capture_output=True,
            text=True,
        )
    except (OSError, FileNotFoundError):
        return None
    if out.returncode != 0:
        return None
    for line in out.stdout.splitlines():
        if line.lower().startswith("status:"):
            return line.split(":", 1)[1].strip()
    return None


def _watchdog_action_arguments() -> str | None:
    try:
        out = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                f"(Get-ScheduledTask -TaskName '{WATCHDOG_TASK}' -ErrorAction SilentlyContinue).Actions[0].Arguments",
            ],
            capture_output=True,
            text=True,
            errors="replace",
        )
    except (OSError, FileNotFoundError):
        return None
    return out.stdout.strip() if out.returncode == 0 and out.stdout.strip() else None


def _private_watchdog_problem(data_dir: Path) -> str | None:
    pointer = data_dir / "active-private-production-release.json"
    if not pointer.is_file():
        return None
    try:
        value = json.loads(pointer.read_text(encoding="utf-8"))
        expected_path = Path(str(value["watchdogScript"])).resolve(strict=True)
        expected_hash = str(value["watchdogSha256"])
        if len(expected_hash) != 64 or any(ch not in "0123456789abcdef" for ch in expected_hash):
            raise ValueError("watchdogSha256 is invalid")
        actual_hash = hashlib.sha256(expected_path.read_bytes()).hexdigest()
        if actual_hash != expected_hash:
            raise ValueError("versioned watchdog hash does not match the active release pointer")
        arguments = _watchdog_action_arguments()
        if arguments is None or str(expected_path).lower() not in arguments.lower():
            raise ValueError("scheduled action does not invoke the active release's versioned watchdog")
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        return f"{PRIVATE_WATCHDOG_TASK} is not bound to the active immutable release: {error}"
    return None


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def _session_reviewers(data_dir: Path) -> int:
    """How many reviewer links the persisted couch session claims are live (0 if no session)."""
    session_file = data_dir / "couch_session.json"
    if not session_file.is_file():
        return 0
    try:
        payload = json.loads(session_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return 0
    reviewers = payload.get("reviewers")
    return len(reviewers) if isinstance(reviewers, (dict, list)) else 0


def _probe(url: str) -> int | None:
    """HTTP status from a fixed localhost URL, or None if nothing answers."""
    # The couch certificate is self-signed by design (rcgen, generated on this machine), so
    # certificate verification is deliberately disabled for this LOCALHOST-only reachability probe.
    # It proves the port is serving; it is not an authentication check, and no data is read.
    ctx = ssl._create_unverified_context() if url.startswith("https") else None
    try:
        with urllib.request.urlopen(url, timeout=8, context=ctx) as resp:  # noqa: S310 — fixed localhost URL
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, OSError, TimeoutError, ssl.SSLError):
        return None


def _couch_status() -> int | None:
    """Reachability of the couch server over EITHER transport.

    The server moved from plain HTTP to TLS on the same port. Probing only one scheme would make this
    gate report "every reviewer link is DEAD" the moment the transport changed — a false alarm on the
    one gate whose entire job is to be believed when it says the links are down. Try HTTPS first
    (the current design), fall back to HTTP (the older build), and treat an answer on either as alive.
    """
    for url in (COUCH_URL_TLS, COUCH_URL):
        status = _probe(url)
        if status is not None:
            return status
    return None


def main() -> int:
    global WATCHDOG_TASK
    if os.name != "nt":
        print("SUPERVISION GATE: SKIP-ENV (Windows rig only — schtasks/AppData)", flush=True)
        return 0

    data_dir = _data_dir()
    if (data_dir / "active-private-production-release.json").is_file():
        WATCHDOG_TASK = PRIVATE_WATCHDOG_TASK
    reviewer_count = _session_reviewers(data_dir)
    floor_gb = float(os.environ.get("CORTEX_DISK_FLOOR_GB", DEFAULT_FLOOR_GB))
    free_bytes = shutil.disk_usage(data_dir if data_dir.exists() else Path.home()).free

    problems = evaluate_supervision(
        watchdog_state=_watchdog_state(),
        watchdog_starts_when_available=_watchdog_starts_when_available(),
        session_expected=reviewer_count > 0,
        reviewer_count=reviewer_count,
        couch_status=_couch_status() if reviewer_count > 0 else None,
        free_bytes=free_bytes,
        floor_bytes=int(floor_gb * 2**30),
    )
    private_problem = _private_watchdog_problem(data_dir)
    if private_problem:
        problems.append(private_problem)

    if problems:
        print("SUPERVISION GATE: FAIL", flush=True)
        for p in problems:
            print(f"  - {p}", flush=True)
        return 1

    print(
        f"SUPERVISION GATE: OK (watchdog enabled, {reviewer_count} reviewer link(s) answering on 8737, "
        f"{free_bytes / 2**30:.1f} GB free)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
