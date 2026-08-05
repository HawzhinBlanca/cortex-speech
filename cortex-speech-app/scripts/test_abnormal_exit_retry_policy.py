"""The sweep must distinguish "the OS killed the harness" from "the test failed".

Three times a Node probe has died with exit 3221226505 (0xC0000409 STATUS_STACK_BUFFER_OVERRUN)
inside a full sweep and never in 103 standalone runs, always before it measured anything. The process
that dies is node.exe — the harness — not the app under test. A leg that produced NO measurement is
not evidence that the app failed its gate, so `run_gate` re-runs once on that signature and keeps a
`<gate>.CRASH.<timestamp>.log` of the dead attempt so the occurrence stays counted.

The danger of any retry is that it turns a REAL regression green. These checks run `run_gate` for
real and pin the boundary: an OS-termination is retried, an ordinary non-zero exit is NOT.
"""

import importlib.util
import os
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
VERIFY = REPO_ROOT / "scripts" / "verify_10.py"


def _load_verify():
    """Import verify_10 as a module without running its CLI."""
    spec = importlib.util.spec_from_file_location("verify_10_under_test", VERIFY)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["verify_10_under_test"] = mod
    spec.loader.exec_module(mod)
    return mod


def test_ordinary_failure_is_not_retried() -> None:
    """A gate that FAILS must fail once and stay failed — the retry must not launder a regression."""
    v = _load_verify()
    marker = Path(tempfile.gettempdir()) / "cortex_retry_policy_counter.txt"
    if marker.exists():
        marker.unlink()
    # Appends a byte per invocation, then exits 1 — an ordinary test failure.
    cmd = f'{sys.executable} -c "open(r\'{marker}\',\'ab\').write(b\'x\'); raise SystemExit(1)"'
    status, _secs, _detail = v.run_gate("retry-policy-fail", "cmd", cmd, str(REPO_ROOT), None, timeout=60)

    runs = len(marker.read_bytes()) if marker.exists() else 0
    marker.unlink(missing_ok=True)
    if runs != 1:
        raise AssertionError(
            f"an ordinary failing gate ran {runs} times — a plain non-zero exit MUST NOT be retried, "
            "or a real regression gets a second chance to pass by luck"
        )
    if status != v.FAIL:
        raise AssertionError(f"an ordinary failing gate reported {status!r}, expected FAIL")


def test_os_termination_is_retried_once_and_leaves_evidence() -> None:
    """The 0xC0000409 signature is retried exactly once, and the dead attempt is preserved."""
    v = _load_verify()
    if 3221226505 not in v.ABNORMAL_EXIT_CODES:
        raise AssertionError("0xC0000409 is no longer treated as an OS termination")

    marker = Path(tempfile.gettempdir()) / "cortex_retry_policy_counter2.txt"
    if marker.exists():
        marker.unlink()
    # A BATCH fixture, not a Python one. Python CLAMPS a large exit value to 0xFFFFFFFF (measured:
    # SystemExit(3221226505) -> returncode 4294967295), and wrapping `cmd /c exit` in Python clamps it
    # again on the way out. The process that must carry the code IS the one the gate launches, so the
    # payload is a .cmd that exits with it directly. Two earlier versions of this fixture silently
    # exercised nothing because of that clamp.
    fixture = Path(tempfile.gettempdir()) / "cortex_retry_policy_fixture.cmd"
    fixture.write_text(
        "@echo off\r\n"
        f'if exist "{marker}" (\r\n'
        f'  echo x>>"{marker}"\r\n'
        "  exit 0\r\n"
        ") else (\r\n"
        f'  echo x>"{marker}"\r\n'
        "  exit 3221226505\r\n"
        ")\r\n",
        encoding="ascii",
    )
    script = str(fixture)
    before = {p.name for p in v.LOG_DIR.glob("retry-policy-crash.CRASH.*.log")} if v.LOG_DIR.exists() else set()
    status, _secs, detail = v.run_gate(
        "retry-policy-crash", "cmd", script, str(REPO_ROOT), None, timeout=60
    )
    runs = len(marker.read_text(encoding="ascii").strip().splitlines()) if marker.exists() else 0
    marker.unlink(missing_ok=True)
    fixture.unlink(missing_ok=True)

    if runs != 2:
        raise AssertionError(
            f"an OS-terminated gate ran {runs} times, expected exactly 2 (one crash + one re-run)"
        )
    if status != v.PASS:
        raise AssertionError(f"the recovered leg reported {status!r}, expected PASS after a clean re-run")
    if "re-ran once" not in detail:
        raise AssertionError(f"the re-run was not disclosed in the gate detail: {detail!r}")

    after = {p.name for p in v.LOG_DIR.glob("retry-policy-crash.CRASH.*.log")}
    if not (after - before):
        raise AssertionError(
            "no CRASH log was kept for the OS-terminated attempt — the occurrence would become "
            "invisible as soon as the retry passed, which is exactly what must not happen"
        )
    for name in after - before:
        (v.LOG_DIR / name).unlink(missing_ok=True)


def main() -> None:
    if os.name != "nt":
        print("abnormal-exit retry policy: skipped (Windows-only exit-code semantics)")
        return
    test_ordinary_failure_is_not_retried()
    test_os_termination_is_retried_once_and_leaves_evidence()
    print("abnormal-exit retry policy passed")


if __name__ == "__main__":
    main()
