"""The readiness gate must keep agreeing with the Rust it claims to reproduce — and must stay refusable.

`check_playback_enforcement_readiness.py` re-implements `db::has_sufficient_playback_evidence` in
Python so the owner can ask "is enforcement safe to turn on?" without a build. A re-implementation
that drifts from the original answers a question nobody asked, so the bar and the policy version are
pinned to the Rust constants here.

The last pin is the one that matters most. The gate exists BECAUSE an empty log read as a pass on
2026-08-19: the binary predated the guard, no phone decision had been taken since receipts began, and
"no reviewer would have been refused" was true only in the way that every statement about an empty
set is true. A gate that cannot return NOT READY on that exact input is the trap it was written to
close, so it is driven against a fabricated-clean database and required to refuse.
"""

from __future__ import annotations

import re
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
GATE = REPO_ROOT / "scripts" / "check_playback_enforcement_readiness.py"
DB_RS = REPO_ROOT / "src-tauri" / "src" / "db.rs"
COUCH_RS = REPO_ROOT / "src-tauri" / "src" / "couch.rs"

sys.path.insert(0, str(GATE.parent))
import check_playback_enforcement_readiness as gate  # noqa: E402


def _rust_const(name: str, source: Path) -> str:
    match = re.search(rf"pub const {name}:\s*\w+\s*=\s*([0-9.]+);", source.read_text(encoding="utf-8"))
    assert match, f"{name} is gone from {source.name} — the gate now pins a constant that no longer exists"
    return match.group(1)


def test_the_coverage_bar_matches_the_rust_constant() -> None:
    assert float(_rust_const("MIN_PLAYBACK_COVERAGE", DB_RS)) == gate.MIN_PLAYBACK_COVERAGE


def test_the_policy_version_matches_the_rust_constant() -> None:
    assert int(_rust_const("PLAYBACK_POLICY_VERSION", DB_RS)) == gate.PLAYBACK_POLICY_VERSION


def test_the_observe_marker_is_the_string_the_server_actually_logs() -> None:
    assert gate.OBSERVE_MARKER.decode() in COUCH_RS.read_text(encoding="utf-8"), (
        "the gate greps the binary for a marker the server no longer emits, so it would pass on silence"
    )


def _seed(db_path: Path) -> None:
    """A database whose only phone decisions are OLD, with no receipts — the 2026-08-19 shape."""
    conn = sqlite3.connect(db_path)
    conn.executescript(
        """
        CREATE TABLE speech_segments (id TEXT PRIMARY KEY, review_revision INTEGER, audio_fingerprint TEXT);
        CREATE TABLE review_events (segment_id TEXT, reviewer TEXT, action TEXT, source TEXT, created_at TEXT);
        CREATE TABLE playback_receipts (segment_id TEXT, segment_revision INTEGER, audio_fingerprint TEXT,
                                        coverage_ratio REAL);
        INSERT INTO speech_segments VALUES ('s1', 0, 'fp1');
        INSERT INTO review_events VALUES ('s1', 'Sara', 'accept', 'couch', '2026-08-18 21:19:20');
        """
    )
    conn.commit()
    conn.close()


def _run(db_path: Path, exe: Path, since: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe), "--since", since],
        capture_output=True,
        text=True,
    )


def test_an_empty_window_is_refused_not_passed() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.OBSERVE_MARKER + b"\x00")  # a binary that CAN warn
        result = _run(db_path, exe, "2026-08-19 05:50:31")
        assert result.returncode == 1, f"an empty window must not read as ready:\n{result.stdout}"
        assert "NOT READY" in result.stdout
        assert "0 decision(s)" in result.stdout


def test_a_binary_that_cannot_warn_is_refused_however_quiet_the_log() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"a binary built before the guard existed")
        result = _run(db_path, exe, "2020-01-01 00:00:00")
        assert result.returncode == 1, f"silence from a build without the guard is not evidence:\n{result.stdout}"
        assert "vacuous" in result.stdout


def test_a_covered_decision_passes_so_the_gate_is_not_merely_a_refuser() -> None:
    """A gate that always says NO is as useless as one that always says YES."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        # TWO reviewers, because the gate's device bar is part of what "ready" means. A positive
        # control that dodges one of the checks is not a control for the gate as configured.
        conn.execute("INSERT INTO speech_segments VALUES ('s2', 0, 'fp2')")
        conn.execute("INSERT INTO review_events VALUES ('s2', 'Hemn', 'edit', 'couch', '2026-08-20 10:00:00')")
        conn.execute("INSERT INTO playback_receipts VALUES ('s1', 0, 'fp1', 0.97)")
        conn.execute("INSERT INTO playback_receipts VALUES ('s2', 0, 'fp2', 0.93)")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.OBSERVE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 0, f"a covered decision must read as ready:\n{result.stdout}"
        assert "READY" in result.stdout


def test_a_receipt_below_the_bar_is_reported_as_a_refusal() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute("INSERT INTO playback_receipts VALUES ('s1', 0, 'fp1', 0.42)")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.OBSERVE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 1
        assert "would have REFUSED 1 of 1" in result.stdout, result.stdout


def test_one_device_is_not_enough_to_enforce_on_eight() -> None:
    """Coverage from a single phone is not evidence about the other seven reviewers' browsers."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute("INSERT INTO playback_receipts VALUES ('s1', 0, 'fp1', 0.99)")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.OBSERVE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 1, "one reviewer must not clear the device bar: " + result.stdout
        assert "only 1 reviewer(s)" in result.stdout, result.stdout


def test_the_default_window_is_utc_like_the_rows_it_filters() -> None:
    """A local-time cutoff against UTC rows hides exactly the most recent work.

    Measured 2026-08-19 on a UTC+3 machine while a reviewer was mid-session: the gate derived its
    cutoff from the exe mtime in local time and compared it to SQLite `datetime('now')` values, which
    are UTC. It reported 0 decisions against 9 real ones -- and concealed the first genuine
    below-bar receipt the observe mode existed to surface.
    """
    import datetime as dt

    with tempfile.TemporaryDirectory() as raw:
        exe = Path(raw) / "cortex-speech-app.exe"
        exe.write_bytes(gate.OBSERVE_MARKER)
        mtime = exe.stat().st_mtime
        expected = dt.datetime.fromtimestamp(mtime, dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(Path(raw) / "missing.db"), "--exe", str(exe)],
            capture_output=True, text=True,
        )
        assert f"decisions at or after {expected}" in result.stdout, (
            "the default window must be UTC, matching the timestamps in the database: " + result.stdout
        )


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PLAYBACK ENFORCEMENT READINESS POLICY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    sys.exit(main())
