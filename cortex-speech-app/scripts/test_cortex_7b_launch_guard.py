#!/usr/bin/env python3
"""Non-live process-ownership tests for the champion startup guard."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

import cortex_7b_launch_guard as launch_guard


SCRIPT = Path(__file__).with_name("cortex_7b_launch_guard.py")
STATE_DIR_ENV = "CORTEX_7B_LAUNCH_STATE_DIR"


def _wait_for(path: Path, timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.02)
    raise AssertionError(f"timed out waiting for {path}")


def _guard_command(token: str, timeout: float, child_code: str) -> list[str]:
    return [
        sys.executable,
        str(SCRIPT),
        "supervise",
        "--token",
        token,
        "--heartbeat-timeout",
        str(timeout),
        "--",
        sys.executable,
        "-c",
        child_code,
    ]


def _signal(environment: dict[str, str], token: str, state: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "signal", "--token", token, "--state", state],
        env=environment,
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )


def test_stale_launcher_heartbeat_kills_the_exact_child() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        state = root / "state"
        survived = root / "survived.txt"
        token = "1" * 32
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(state)
        child_code = (
            "import pathlib,time;"
            "time.sleep(1.0);"
            f"pathlib.Path({str(survived)!r}).write_text('orphan',encoding='utf-8');"
            "time.sleep(30)"
        )
        guard = subprocess.Popen(
            _guard_command(token, 0.25, child_code),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        _wait_for(state / f"{token}.pid")
        stdout, stderr = guard.communicate(timeout=5)
        assert guard.returncode == 124, (stdout, stderr)
        time.sleep(1.0)
        assert not survived.exists(), "heartbeat expiry left the child alive"
        assert not (state / f"{token}.pid").exists()
        assert "heartbeat expired before READY" in (state / f"{token}.log").read_text(encoding="utf-8")


def test_ready_transfer_keeps_the_child_until_its_normal_exit() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        state = root / "state"
        completed = root / "completed.txt"
        token = "2" * 32
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(state)
        child_code = (
            "import pathlib,time;"
            "time.sleep(0.7);"
            f"pathlib.Path({str(completed)!r}).write_text('complete',encoding='utf-8')"
        )
        guard = subprocess.Popen(
            _guard_command(token, 0.3, child_code),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        _wait_for(state / f"{token}.pid")
        signal_result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "signal",
                "--token",
                token,
                "--state",
                "ready",
            ],
            env=environment,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        assert signal_result.returncode == 0, signal_result.stderr
        stdout, stderr = guard.communicate(timeout=5)
        assert guard.returncode == 0, (stdout, stderr)
        assert completed.read_text(encoding="utf-8") == "complete"
        assert "READY ownership transferred" in (state / f"{token}.log").read_text(encoding="utf-8")
        assert not (state / f"{token}.pid").exists()


def test_ready_without_a_live_guard_fails_closed() -> None:
    with tempfile.TemporaryDirectory() as name:
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(Path(name) / "state")
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "signal",
                "--token",
                "3" * 32,
                "--state",
                "ready",
            ],
            env=environment,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        assert result.returncode == 2
        assert "before the launch guard publishes its child PID" in result.stderr


def test_exact_stop_request_reaps_the_child() -> None:
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        state = root / "state"
        survived = root / "survived.txt"
        token = "4" * 32
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(state)
        child_code = (
            "import pathlib,time;"
            "time.sleep(1.0);"
            f"pathlib.Path({str(survived)!r}).write_text('orphan',encoding='utf-8');"
            "time.sleep(30)"
        )
        guard = subprocess.Popen(
            _guard_command(token, 5.0, child_code),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        _wait_for(state / f"{token}.pid")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "signal", "--token", token, "--state", "stop"],
            env=environment,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        stdout, stderr = guard.communicate(timeout=5)
        assert guard.returncode == 125, (stdout, stderr)
        time.sleep(1.0)
        assert not survived.exists(), "exact stop left the child alive"
        assert "exact stop request" in (state / f"{token}.log").read_text(encoding="utf-8")


def test_high_volume_output_is_drained_rotated_and_tail_read_live() -> None:
    """Many pipe capacities of output cannot deadlock the child or exceed two bounded segments."""
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        state = root / "state"
        completed_output = root / "output-completed.txt"
        token = "5" * 32
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(state)
        child_code = (
            "import os,pathlib,time;"
            "chunk=b'volume-'*8192;"
            "[os.write(1,chunk) for _ in range(192)];"
            "os.write(1,b'\\nFINAL-DIAGNOSTIC-TAIL\\n');"
            f"pathlib.Path({str(completed_output)!r}).write_text('drained',encoding='utf-8');"
            "time.sleep(30)"
        )
        guard = subprocess.Popen(
            _guard_command(token, 10.0, child_code),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        prior_state_dir = os.environ.get(STATE_DIR_ENV)
        os.environ[STATE_DIR_ENV] = str(state)
        tail_errors: list[BaseException] = []
        stop_tailer = threading.Event()
        final_lines: list[str] = []

        def sample_live_tail() -> None:
            while not stop_tailer.is_set():
                try:
                    launch_guard.read_tail_lines(token, 10)
                except BaseException as exc:  # surfaced on the test thread below
                    tail_errors.append(exc)
                    return
                time.sleep(0.005)

        tailer = threading.Thread(target=sample_live_tail, daemon=True)
        try:
            _wait_for(state / f"{token}.pid")
            ready = _signal(environment, token, "ready")
            assert ready.returncode == 0, ready.stderr
            tailer.start()
            _wait_for(completed_output, timeout=15.0)
            deadline = time.monotonic() + 5.0
            lines: list[str] = []
            while time.monotonic() < deadline:
                lines = launch_guard.read_tail_lines(token, 10)
                if any("FINAL-DIAGNOSTIC-TAIL" in line for line in lines):
                    break
                time.sleep(0.02)
            assert any("FINAL-DIAGNOSTIC-TAIL" in line for line in lines), lines
            assert guard.poll() is None, "bounded rotation disrupted the active child"
            sizes = [
                path.stat().st_size
                for path in (state / f"{token}.log", state / f"{token}.log.1")
                if path.exists()
            ]
            assert sizes and all(size <= launch_guard.MAX_LOG_SEGMENT_BYTES for size in sizes), sizes
            assert sum(sizes) <= 2 * launch_guard.MAX_LOG_SEGMENT_BYTES, sizes
            assert not tail_errors, tail_errors
        finally:
            stop_tailer.set()
            if tailer.ident is not None:
                tailer.join(timeout=2)
            if guard.poll() is None:
                _signal(environment, token, "stop")
            stdout, stderr = guard.communicate(timeout=20)
            final_lines = launch_guard.read_tail_lines(token, 10)
            if prior_state_dir is None:
                os.environ.pop(STATE_DIR_ENV, None)
            else:
                os.environ[STATE_DIR_ENV] = prior_state_dir
        assert guard.returncode == 125, (stdout, stderr)
        assert "FINAL-DIAGNOSTIC-TAIL" in "\n".join(final_lines)


def test_pruning_bounds_stale_attempts_without_deleting_a_live_attempt() -> None:
    """Age/count cleanup rechecks the exact live PID and preserves an active ancient-looking log."""
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        state = root / "state"
        token = "6" * 32
        environment = os.environ.copy()
        environment[STATE_DIR_ENV] = str(state)
        guard = subprocess.Popen(
            _guard_command(token, 10.0, "import time;print('LIVE',flush=True);time.sleep(30)"),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        prior_state_dir = os.environ.get(STATE_DIR_ENV)
        os.environ[STATE_DIR_ENV] = str(state)
        try:
            _wait_for(state / f"{token}.pid")
            ready = _signal(environment, token, "ready")
            assert ready.returncode == 0, ready.stderr
            _wait_for(state / f"{token}.log")
            now = time.time()
            ancient = now - launch_guard.STALE_ATTEMPT_MAX_AGE_SECONDS - 60
            os.utime(state / f"{token}.pid", (ancient, ancient))
            os.utime(state / f"{token}.log", (ancient, ancient))

            old_tokens = [f"{100 + index:032x}" for index in range(3)]
            for stale_token in old_tokens:
                for suffix in ("log", "log.1", "heartbeat"):
                    path = state / f"{stale_token}.{suffix}"
                    path.write_text("stale", encoding="ascii")
                    os.utime(path, (ancient, ancient))
            dead_pid_token = f"{200:032x}"
            dead_pid = state / f"{dead_pid_token}.pid"
            dead_pid.write_text("99999999", encoding="ascii")
            dead_log = state / f"{dead_pid_token}.log"
            dead_log.write_text("dead", encoding="ascii")
            os.utime(dead_pid, (ancient, ancient))
            os.utime(dead_log, (ancient, ancient))

            recent_tokens = [f"{300 + index:032x}" for index in range(20)]
            for index, recent_token in enumerate(recent_tokens):
                path = state / f"{recent_token}.log"
                path.write_text("recent", encoding="ascii")
                age = launch_guard.PRUNE_SAFETY_AGE_SECONDS + 60 + index
                os.utime(path, (now - age, now - age))

            removed = launch_guard.prune_stale_attempts(state, now=now)
            assert removed > 0
            assert guard.poll() is None, "pruning disrupted the active child"
            assert (state / f"{token}.pid").exists()
            assert (state / f"{token}.log").exists()
            assert all(not any(state.glob(f"{stale_token}.*")) for stale_token in old_tokens)
            assert not dead_pid.exists() and not dead_log.exists()
            retained_recent = [token for token in recent_tokens if (state / f"{token}.log").exists()]
            assert len(retained_recent) == launch_guard.MAX_RETAINED_INACTIVE_ATTEMPTS
        finally:
            if guard.poll() is None:
                _signal(environment, token, "stop")
            stdout, stderr = guard.communicate(timeout=20)
            if prior_state_dir is None:
                os.environ.pop(STATE_DIR_ENV, None)
            else:
                os.environ[STATE_DIR_ENV] = prior_state_dir
        assert guard.returncode == 125, (stdout, stderr)


if __name__ == "__main__":
    test_stale_launcher_heartbeat_kills_the_exact_child()
    test_ready_transfer_keeps_the_child_until_its_normal_exit()
    test_ready_without_a_live_guard_fails_closed()
    test_exact_stop_request_reaps_the_child()
    test_high_volume_output_is_drained_rotated_and_tail_read_live()
    test_pruning_bounds_stale_attempts_without_deleting_a_live_attempt()
    print("PASS: champion launch guard ownership + bounded logging (6 tests)")
