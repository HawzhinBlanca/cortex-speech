#!/usr/bin/env python3
"""Regression tests for the local owner-workstation health boundary."""

from __future__ import annotations

import check_owner_workstation_health_live as health


def test_healthy_owner_workstation_has_no_remote_reviewer_dependency() -> None:
    assert health.evaluate_owner_workstation(
        watchdog_state="Ready",
        watchdog_starts_when_available=True,
        watchdog_has_current_repetition=True,
        free_bytes=40 * 2**30,
        floor_bytes=20 * 2**30,
        private_release_problem=None,
    ) == []


def test_every_local_safety_failure_remains_blocking() -> None:
    problems = health.evaluate_owner_workstation(
        watchdog_state="Disabled",
        watchdog_starts_when_available=False,
        watchdog_has_current_repetition=False,
        free_bytes=2 * 2**30,
        floor_bytes=20 * 2**30,
        private_release_problem="active release watchdog hash mismatch",
    )
    combined = "\n".join(problems)
    assert "DISABLED" in combined
    assert "only 2.0 GB free" in combined
    assert "active release watchdog hash mismatch" in combined


def main() -> int:
    test_healthy_owner_workstation_has_no_remote_reviewer_dependency()
    test_every_local_safety_failure_remains_blocking()
    print("owner workstation health policy tests: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
