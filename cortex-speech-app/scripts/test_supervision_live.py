#!/usr/bin/env python3
"""Unit tests for the supervision gate's pure decision core.

CI-safe: no schtasks, no HTTP, no disk inspection — every input is passed in. The cases are the
exact machine state of 2026-08-15, when all three failures were live at once and every source-level
gate in the sweep was still capable of reporting GREEN.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_supervision_live import evaluate_supervision  # noqa: E402

GB = 2**30
HEALTHY = dict(
    watchdog_state="Ready",
    watchdog_has_current_repetition=True,
    session_expected=True,
    reviewer_count=5,
    couch_status=200,
    free_bytes=94 * GB,
    floor_bytes=20 * GB,
)


def test_healthy_machine_passes():
    assert evaluate_supervision(**HEALTHY) == []


def test_disabled_watchdog_fails():
    problems = evaluate_supervision(**{**HEALTHY, "watchdog_state": "Disabled"})
    assert len(problems) == 1, problems
    assert "DISABLED" in problems[0]


def test_unregistered_watchdog_fails():
    problems = evaluate_supervision(**{**HEALTHY, "watchdog_state": None})
    assert len(problems) == 1, problems
    assert "not registered" in problems[0]


def test_a_watchdog_that_cannot_wake_the_machine_fails():
    """REGISTERED, ENABLED, AND STILL USELESS AFTER A SLEEP (2026-08-18).

    Windows DROPS a repetition occurrence that comes due while the machine is asleep unless
    StartWhenAvailable is set — it does not run it late. cortex-watchdog.ps1 registers the task WITH
    that flag, but the LIVE task on the owner's machine had drifted to False and nothing looked.
    Measured from watchdog.log: 19 clean-exit relaunches, median ~8 min down, worst 9 h 18 m — and the
    couch server lives inside the app, so each window is every reviewer link dead.
    """
    problems = evaluate_supervision(**{**HEALTHY, "watchdog_starts_when_available": False})
    assert len(problems) == 1, problems
    assert "StartWhenAvailable" in problems[0]
    assert "9 h 18 m" in problems[0], "the message must carry the measured cost, not just the flag name"


def test_an_unreadable_start_when_available_is_not_a_failure():
    """None means the probe could not ask (no PowerShell, not Windows). Degrade, never false-alarm."""
    assert evaluate_supervision(**{**HEALTHY, "watchdog_starts_when_available": None}) == []


def test_a_logon_only_watchdog_registered_after_sign_in_fails():
    problems = evaluate_supervision(**{**HEALTHY, "watchdog_has_current_repetition": False})
    assert len(problems) == 1, problems
    assert "no active five-minute clock trigger" in problems[0]


def test_an_unreadable_repetition_probe_is_not_a_failure():
    assert evaluate_supervision(**{**HEALTHY, "watchdog_has_current_repetition": None}) == []


def test_a_disabled_watchdog_outranks_the_drift_check():
    """Disabled is the worse problem and must be the one reported — not two confusing messages."""
    problems = evaluate_supervision(
        **{
            **HEALTHY,
            "watchdog_state": "Disabled",
            "watchdog_starts_when_available": False,
            "watchdog_has_current_repetition": False,
        }
    )
    assert len(problems) == 1, problems
    assert "DISABLED" in problems[0]


def test_live_links_with_a_dead_server_fails():
    """The 2026-08-15 case: five links sent, app exited, nothing listening."""
    problems = evaluate_supervision(**{**HEALTHY, "couch_status": None})
    assert len(problems) == 1, problems
    assert "DEAD" in problems[0]


def test_non_200_from_the_couch_server_fails():
    problems = evaluate_supervision(**{**HEALTHY, "couch_status": 503})
    assert len(problems) == 1, problems
    assert "503" in problems[0]


def test_no_session_means_a_dead_port_is_not_a_failure():
    """No links have been handed out, so nothing is broken by the app being closed."""
    assert evaluate_supervision(**{**HEALTHY, "session_expected": False, "reviewer_count": 0, "couch_status": None}) == []


def test_full_disk_fails():
    """0 bytes free is what broke the 10-minute snapshot safety net while the app reported healthy."""
    problems = evaluate_supervision(**{**HEALTHY, "free_bytes": 0})
    assert len(problems) == 1, problems
    assert "snapshot safety net" in problems[0]


def test_disk_exactly_at_the_floor_passes():
    """Boundary: the floor is a minimum, not a strict inequality trap."""
    assert evaluate_supervision(**{**HEALTHY, "free_bytes": 20 * GB}) == []


def test_all_three_failures_are_reported_together():
    """A gate that stops at the first problem hides the other two and costs a second round trip."""
    problems = evaluate_supervision(
        **{**HEALTHY, "watchdog_state": "Disabled", "couch_status": None, "free_bytes": 0}
    )
    assert len(problems) == 3, problems


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"SUPERVISION GATE CORE: {len(tests)} assertions passed")
