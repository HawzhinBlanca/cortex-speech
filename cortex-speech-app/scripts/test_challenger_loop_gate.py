#!/usr/bin/env python3
"""Pins for gate D's audits — a run or verdict that looks finished when it is not.

A REJECT verdict is a PASS here: the loop's job is to decide honestly, not to win. What must never
pass is a record that claims more than it did.
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_challenger_loop as loop_gate  # noqa: E402
from check_challenger_loop import audit_run, audit_verdict, completion_reasons  # noqa: E402
from train_challenger import RUNS_DIR as TRAIN_RUNS_DIR  # noqa: E402

SNAP = "a" * 64


def test_a_prepared_run_is_honest() -> None:
    assert audit_run({"status": "prepared", "snapshot_id": SNAP}) == []


def test_a_trained_run_needs_a_trainer() -> None:
    """'trained' with nothing that could have trained it is the flattering-summary failure."""
    problems = audit_run({"status": "trained", "snapshot_id": SNAP})
    assert any("could not have happened" in p or "no trainer_command" in p for p in problems), problems


def test_a_failed_trainer_may_not_be_recorded_as_trained() -> None:
    problems = audit_run(
        {"status": "trained", "snapshot_id": SNAP, "trainer_command": "x", "trainer_exit_code": 1}
    )
    assert any("trainer exit" in p for p in problems), problems


def test_a_run_without_a_snapshot_fails() -> None:
    problems = audit_run({"status": "prepared"})
    assert any("no snapshot_id" in p for p in problems), problems


def test_a_prepared_run_claiming_trainer_success_fails() -> None:
    problems = audit_run({"status": "prepared", "snapshot_id": SNAP, "trainer_exit_code": 0})
    assert any("one of the two is wrong" in p for p in problems), problems


def test_a_reject_verdict_passes() -> None:
    """The expected outcome on today's data, and a perfectly good one."""
    assert audit_verdict(
        {"verdict": "REJECT", "champion_cer": 0.2, "challenger_cer": 0.3, "paired_clips": 40}
    ) == []


def test_a_reject_without_measurement_is_not_a_completed_verdict() -> None:
    problems = audit_verdict({"verdict": "REJECT"})
    assert any("measured CER" in problem for problem in problems), problems
    assert any("no paired clips" in problem for problem in problems), problems


def test_a_promote_contradicting_its_own_numbers_fails() -> None:
    problems = audit_verdict(
        {"verdict": "PROMOTE", "champion_cer": 0.20, "challenger_cer": 0.25, "paired_clips": 40}
    )
    assert any("not better" in p for p in problems), problems


def test_a_promote_over_a_regressed_slice_fails() -> None:
    """Defence in depth: the gate refuses this, and this audit catches a verdict that slipped through."""
    problems = audit_verdict(
        {
            "verdict": "PROMOTE",
            "champion_cer": 0.20,
            "challenger_cer": 0.10,
            "paired_clips": 40,
            "slices": [{"slice": "dialect:badini", "champion_cer": 0.2, "challenger_cer": 0.4}],
        }
    )
    assert any("regressed" in p for p in problems), problems


def test_a_promote_with_nothing_compared_fails() -> None:
    problems = audit_verdict({"verdict": "PROMOTE", "champion_cer": 0.2, "challenger_cer": 0.1})
    assert any("no paired clips" in p for p in problems), problems


def test_exit_zero_with_no_artifacts_is_not_a_trained_run() -> None:
    problems = audit_run(
        {
            "schema": 2,
            "status": "trained",
            "snapshot_id": SNAP,
            "trainer_command": "fake-success",
            "trainer_exit_code": 0,
        }
    )
    assert any("declares no real artifacts" in problem for problem in problems), problems
    assert any("artifact_manifest" in problem for problem in problems), problems


def test_zero_trained_is_incomplete_and_non_green() -> None:
    assert "zero verified trained challengers" in completion_reasons(0, 1, 0, 0)


def test_zero_verdict_is_incomplete_and_non_green() -> None:
    assert "zero measured verdicts" in completion_reasons(1, 0, 0, 1)


def test_a_trained_run_without_linked_verdict_is_incomplete() -> None:
    reasons = completion_reasons(1, 1, 0, 1)
    assert any("no verdict is byte-linked" in reason for reason in reasons), reasons
    assert any("no linked verdict" in reason for reason in reasons), reasons


def test_empty_loop_main_returns_nonzero_incomplete() -> None:
    with tempfile.TemporaryDirectory() as raw:
        original = loop_gate.RUNS_DIR
        loop_gate.RUNS_DIR = Path(raw)
        try:
            assert loop_gate.main() == 1
        finally:
            loop_gate.RUNS_DIR = original


def test_trainer_and_gate_use_the_same_default_runs_directory() -> None:
    assert TRAIN_RUNS_DIR == loop_gate.RUNS_DIR


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"CHALLENGER LOOP GATE: {len(tests)} audits pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
