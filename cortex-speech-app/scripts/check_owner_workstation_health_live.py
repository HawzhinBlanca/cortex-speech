#!/usr/bin/env python3
"""Fail-closed live health gate for the single-user owner workstation.

This intentionally does not inspect Couch sessions, reviewer links, queue depth, or compensation.
Those are a separately operated service. The owner-product profile still needs the local safety
parts that used to be coupled to that service: an enabled current watchdog, a watchdog bound to the
active immutable release when that pointer exists, and enough free space for SQLite/WAL/snapshots.
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

import check_supervision_live as supervision


def evaluate_owner_workstation(
    *,
    watchdog_state: str | None,
    watchdog_starts_when_available: bool | None,
    watchdog_has_current_repetition: bool | None,
    free_bytes: int,
    floor_bytes: int,
    private_release_problem: str | None,
) -> list[str]:
    """Return local owner-workstation blockers without consulting reviewer-service state."""

    problems = supervision.evaluate_supervision(
        watchdog_state=watchdog_state,
        watchdog_starts_when_available=watchdog_starts_when_available,
        watchdog_has_current_repetition=watchdog_has_current_repetition,
        session_expected=False,
        reviewer_count=0,
        couch_status=None,
        free_bytes=free_bytes,
        floor_bytes=floor_bytes,
    )
    if private_release_problem is not None:
        problems.append(private_release_problem)
    return problems


def main() -> int:
    if os.name != "nt":
        print("OWNER WORKSTATION HEALTH: FAIL (owner-product requires Windows 11)", flush=True)
        return 1

    data_dir = supervision._data_dir()
    if (data_dir / "active-private-production-release.json").is_file():
        supervision.WATCHDOG_TASK = supervision.PRIVATE_WATCHDOG_TASK
    else:
        supervision.WATCHDOG_TASK = supervision.LEGACY_WATCHDOG_TASK

    try:
        floor_gb = float(os.environ.get("CORTEX_DISK_FLOOR_GB", supervision.DEFAULT_FLOOR_GB))
    except ValueError:
        print("OWNER WORKSTATION HEALTH: FAIL (CORTEX_DISK_FLOOR_GB is invalid)", flush=True)
        return 1
    if not 1.0 <= floor_gb <= 1024.0:
        print("OWNER WORKSTATION HEALTH: FAIL (disk floor must be between 1 and 1024 GiB)", flush=True)
        return 1

    probe_root = data_dir if data_dir.exists() else Path.home()
    try:
        free_bytes = shutil.disk_usage(probe_root).free
    except OSError as error:
        print(f"OWNER WORKSTATION HEALTH: FAIL (cannot inspect data drive: {error})", flush=True)
        return 1

    problems = evaluate_owner_workstation(
        watchdog_state=supervision._watchdog_state(),
        watchdog_starts_when_available=supervision._watchdog_starts_when_available(),
        watchdog_has_current_repetition=supervision._watchdog_has_current_repetition(),
        free_bytes=free_bytes,
        floor_bytes=int(floor_gb * 2**30),
        private_release_problem=supervision._private_watchdog_problem(data_dir),
    )
    if problems:
        print("OWNER WORKSTATION HEALTH: FAIL", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 1

    print(
        "OWNER WORKSTATION HEALTH: OK "
        f"(watchdog enabled and current, {free_bytes / 2**30:.1f} GiB free)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
