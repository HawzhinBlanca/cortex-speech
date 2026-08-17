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
