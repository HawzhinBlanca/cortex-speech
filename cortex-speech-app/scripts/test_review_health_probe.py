"""Execute the Windows health supervisor against isolated, non-networked gate fixtures.

No real reviewer tokens, database, task registration or desktop alert paths are used.
These tests exercise the actual PowerShell script, not a second implementation of it.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


PROBE = Path(__file__).parent / "ops" / "review-health-probe.ps1"
GATES = (
    "check_reviewer_links_live.py",
    "check_reviewer_queues_live.py",
    "check_reviewer_link_continuity.py",
    "reviewer_link_vault.py",
)


def fixture(root: Path) -> tuple[Path, Path, Path]:
    app = root / "app with spaces [fixture]"
    scripts = app / "scripts"
    (scripts / "ops").mkdir(parents=True)
    probe = scripts / "ops" / PROBE.name
    shutil.copyfile(PROBE, probe)
    for name in GATES:
        (scripts / name).write_text("print('isolated gate OK')\n", encoding="utf-8")
    # A real green requires the stronger production arguments, not an empty-roster pass.
    (scripts / GATES[0]).write_text(
        "import sys\n"
        "assert set(sys.argv[1:]) == {'--funnel', '--require-links', '--require-private-production'}\n"
        "print('isolated link gate OK')\n", encoding="utf-8",
    )
    logs = root / "health logs [fixture]"
    logs.mkdir()
    (logs / "review-health.json").write_text('{"ok":true,"detail":"old green"}', encoding="utf-8")
    return probe, logs, root / "alert [fixture].txt"


def run(
    probe: Path, logs: Path, alert: Path, python: Path | None = None, *, timeout_seconds: int = 5,
) -> subprocess.CompletedProcess:
    env = os.environ.copy()
    # Every subprocess capture lives under this fixture, including timeout residue checks.
    env["TEMP"] = str(logs.parent)
    return subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
         "-File", str(probe), "-PythonPath", str(python or Path(sys.executable)),
         "-LogDirectory", str(logs), "-AlertPath", str(alert),
         "-GateTimeoutSeconds", str(timeout_seconds), "-NoPopup"],
        capture_output=True, text=True, timeout=35, env=env,
    )


def assert_red(result: subprocess.CompletedProcess, logs: Path, alert: Path, reason: str) -> None:
    assert result.returncode == 1, result.stdout + result.stderr
    state = json.loads((logs / "review-health.json").read_text(encoding="utf-8-sig"))
    assert state["ok"] is False, state
    assert reason in state["detail"], state
    assert reason in alert.read_text(encoding="utf-8-sig")
    assert reason in (logs / "review-health.log").read_text(encoding="utf-8-sig")
    assert not list(logs.parent.glob("review-health-*.out"))
    assert not list(logs.parent.glob("review-health-*.err"))


def test_missing_interpreter_replaces_previous_green_with_visible_failure() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        result = run(probe, logs, alert, Path(raw) / "missing-python.exe")
        assert_red(result, logs, alert, "locked interpreter missing")


def test_unlaunchable_interpreter_is_a_reported_failure() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        invalid = Path(raw) / "invalid-python.exe"
        invalid.write_bytes(b"not a Windows executable")
        result = run(probe, logs, alert, invalid)
        assert_red(result, logs, alert, "probe failed")


def test_timeout_is_red_and_stops_only_the_fixture_gate() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        (probe.parent.parent / GATES[0]).write_text(
            "import time, pathlib, os\n"
            "pathlib.Path('gate.pid').write_text(str(os.getpid()))\n"
            "time.sleep(20)\n"
            "pathlib.Path('must-not-finish').write_text('orphan')\n", encoding="utf-8",
        )
        result = run(probe, logs, alert, timeout_seconds=1)
        assert_red(result, logs, alert, "TIMED OUT")
        app = probe.parent.parent.parent
        pid = int((app / "gate.pid").read_text())
        alive = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
             f"if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}"],
            capture_output=True, text=True, timeout=10,
        )
        assert alive.returncode == 0, "timed-out gate process survived"
        assert not (app / "must-not-finish").exists()


def test_gate_failure_then_recovery_updates_state_and_clears_alert() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        gate = probe.parent.parent / GATES[1]
        gate.write_text("print('fixture queue failure')\nraise SystemExit(9)\n", encoding="utf-8")
        assert_red(run(probe, logs, alert), logs, alert, "queues exit=9")
        gate.write_text("print('recovered queue')\n", encoding="utf-8")
        result = run(probe, logs, alert)
        assert result.returncode == 0, result.stdout + result.stderr
        state = json.loads((logs / "review-health.json").read_text(encoding="utf-8-sig"))
        assert state["ok"] is True, state
        assert "recovered queue" in state["detail"]
        assert not alert.exists()
        assert "RECOVERED" in (logs / "review-health.log").read_text(encoding="utf-8-sig")


def test_verbose_gate_drains_both_pipes_without_deadlock() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        (probe.parent.parent / GATES[1]).write_text(
            "import sys\n"
            "sys.stdout.write('out' * 50000 + '\\n')\n"
            "sys.stderr.write('err' * 50000 + '\\nfixture verbose failure\\n')\n"
            "raise SystemExit(7)\n", encoding="utf-8",
        )
        assert_red(run(probe, logs, alert), logs, alert, "queues exit=7")
        assert len(alert.read_text(encoding="utf-8-sig")) < 4000


def test_unicode_diagnostics_are_preserved() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, logs, alert = fixture(Path(raw))
        message = 'fixture: دەنگ — unavailable'
        (probe.parent.parent / GATES[1]).write_text(
            f"print({message!r})\nraise SystemExit(8)\n", encoding="utf-8",
        )
        assert_red(run(probe, logs, alert), logs, alert, message)


def test_broken_log_destination_still_publishes_desktop_failure() -> None:
    with tempfile.TemporaryDirectory() as raw:
        probe, _logs, alert = fixture(Path(raw))
        occupied = Path(raw) / "occupied-log-path"
        occupied.write_text("fixture file blocks directory creation", encoding="utf-8")
        result = run(probe, occupied, alert)
        assert result.returncode == 1, result.stdout + result.stderr
        message = alert.read_text(encoding="utf-8-sig")
        assert "probe crashed" in message, message + result.stdout + result.stderr
        assert occupied.read_text(encoding="utf-8") == "fixture file blocks directory creation"


def main() -> int:
    if sys.platform != "win32":
        print("review health runtime fixtures skipped: the deployed supervisor is Windows-only")
        return 0
    tests = [fn for name, fn in sorted(globals().items()) if name.startswith("test_") and callable(fn)]
    for test in tests:
        test()
        print(f"PASS {test.__name__}", flush=True)
    print(f"review health runtime: {len(tests)} cases passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
