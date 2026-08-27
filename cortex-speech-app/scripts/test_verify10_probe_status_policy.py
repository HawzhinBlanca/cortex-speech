"""A verify-10 probe must be able to say FAIL, not only "environment not ready".

`run_gate` used to map ANY probe reason to SKIP-ENV. `_probe_champion_7b` was strengthened on
2026-08-20 to speak the champion protocol and refuse a wrong model on the champion port — but it
reports through that same channel, so "the WRONG MODEL is answering the champion port" came out as
SKIP-ENV: the operator reads "my server isn't up", and the 494/494-wrong-engine incident class gets
the friendliest status in the suite. (It can never mint a green — a skip forces INCOMPLETE — which is
exactly why it survived unnoticed.)

Pinned here: a probe returns None, a reason string (SKIP-ENV), or an explicit (status, reason) pair;
the champion probe uses the pair for identity mismatch; a crashing probe fails THAT gate instead of
aborting the sweep; and the LNK1104 retry keeps both attempts in the log as its comment promises.
"""

import functools
import importlib.util
import json
import os
import socket
import sys
import tempfile
import threading
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
VERIFY = REPO_ROOT / "scripts" / "verify_10.py"

PINNED_SHA = "a" * 64
WRONG_SHA = "b" * 64


@functools.lru_cache(maxsize=1)
def _load_verify():
    """Import verify_10 as a module without running its CLI (once: import mutates NODE_OPTIONS)."""
    spec = importlib.util.spec_from_file_location("verify_10_probe_status", VERIFY)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["verify_10_probe_status"] = mod
    spec.loader.exec_module(mod)
    return mod


def _health_server(reply: bytes) -> tuple[int, threading.Thread, socket.socket]:
    """One-shot listener that answers a single {"op": "health"} line with `reply`."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]

    def serve():
        try:
            conn, _ = srv.accept()
            with conn:
                conn.recv(4096)
                conn.sendall(reply + b"\n")
        except OSError:
            pass

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    return port, t, srv


def _probe_against(reply: bytes):
    """Run `_probe_champion_7b` against a fake server and a fake live champion pin."""
    v = _load_verify()
    port, thread, srv = _health_server(reply)
    appdata = Path(tempfile.mkdtemp(prefix="cortex_probe_status_"))
    pointer = appdata / "cortex-speech" / "champion.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    pointer.write_text(
        json.dumps({"champions": {"omniasr-7b": {"deploymentSha256": PINNED_SHA}}}), encoding="utf-8"
    )
    saved = {k: os.environ.get(k) for k in ("CORTEX_7B_PORT", "APPDATA")}
    os.environ["CORTEX_7B_PORT"] = str(port)
    os.environ["APPDATA"] = str(appdata)
    try:
        return v, v._probe_champion_7b()
    finally:
        for k, old in saved.items():
            if old is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = old
        srv.close()
        thread.join(timeout=5)


def test_a_wrong_model_on_the_champion_port_is_a_failure_not_a_skip() -> None:
    v, verdict = _probe_against(
        json.dumps({"status": "ready", "deploymentSha256": WRONG_SHA}).encode("utf-8")
    )
    if not isinstance(verdict, tuple) or verdict[0] != v.FAIL:
        raise AssertionError(
            f"identity mismatch reported {verdict!r} — a wrong model answering the champion port must "
            "be FAIL, not 'environment not ready'"
        )
    if "WRONG MODEL" not in verdict[1]:
        raise AssertionError(f"the reason no longer names the wrong model: {verdict[1]!r}")


def test_an_unparseable_health_reply_is_a_failure_not_a_skip() -> None:
    v, verdict = _probe_against(b"not json at all")
    if not isinstance(verdict, tuple) or verdict[0] != v.FAIL:
        raise AssertionError(
            f"a non-champion-protocol reply reported {verdict!r} — something IS listening on the "
            "champion port and it is not the champion; that is FAIL"
        )


def test_the_matching_pin_still_passes_the_probe() -> None:
    """The FAIL path must not swallow the healthy case."""
    _v, verdict = _probe_against(
        json.dumps({"status": "ready", "deploymentSha256": PINNED_SHA}).encode("utf-8")
    )
    if verdict is not None:
        raise AssertionError(f"the pinned champion answering correctly must be runnable, got {verdict!r}")


def test_a_server_that_is_down_still_skips() -> None:
    """Genuine machine state stays SKIP-ENV — this fix must not turn 'not up' into a red gate."""
    v = _load_verify()
    free = socket.socket()
    free.bind(("127.0.0.1", 0))
    port = free.getsockname()[1]
    free.close()  # nothing listens there now
    saved = os.environ.get("CORTEX_7B_PORT")
    os.environ["CORTEX_7B_PORT"] = str(port)
    try:
        verdict = v._probe_champion_7b()
    finally:
        if saved is None:
            os.environ.pop("CORTEX_7B_PORT", None)
        else:
            os.environ["CORTEX_7B_PORT"] = saved
    if not isinstance(verdict, str) or "not up" not in verdict:
        raise AssertionError(f"an absent server must be a plain SKIP-ENV reason, got {verdict!r}")
    status, _secs, _detail = v.run_gate("probe-status-skip", "fn", lambda: True, None, lambda: verdict)
    if status != v.SKIP_ENV:
        raise AssertionError(f"a reason string must still mean {v.SKIP_ENV}, got {status!r}")


def test_run_gate_carries_the_probe_status_through() -> None:
    v = _load_verify()
    ran = []
    status, _secs, detail = v.run_gate(
        "probe-status-fail", "fn", lambda: ran.append(1) or True, None, lambda: (v.FAIL, "wrong model")
    )
    if status != v.FAIL or detail != "wrong model":
        raise AssertionError(f"a (FAIL, reason) probe verdict became {status!r}/{detail!r}")
    if ran:
        raise AssertionError("the gate body ran despite the probe refusing it")


def test_a_crashing_probe_fails_only_its_own_gate() -> None:
    """An exception inside one probe used to abort every remaining gate in the sweep."""
    v = _load_verify()

    def boom():
        raise RuntimeError("probe exploded")

    status, _secs, detail = v.run_gate("probe-status-crash", "fn", lambda: True, None, boom)
    if status != v.FAIL or "probe crashed" not in detail:
        raise AssertionError(f"a crashing probe reported {status!r}/{detail!r}, expected a FAIL for that gate")


def test_the_lnk1104_retry_keeps_both_attempts_in_the_log() -> None:
    """The comment promised both attempts; the code kept only the second."""
    v = _load_verify()
    marker = Path(tempfile.gettempdir()) / "cortex_probe_status_lnk.txt"
    marker.unlink(missing_ok=True)
    body = (
        "import os,sys;"
        f"p=r'{marker}';"
        "first=not os.path.exists(p);"
        "open(p,'w').write('x');"
        "print('ATTEMPT1 LNK1104' if first else 'ATTEMPT2');"
        "sys.exit(1 if first else 0)"
    )
    cmd = f'{sys.executable} -c "{body}"'
    try:
        status, _secs, _detail = v.run_gate("probe-status-lnk", "cmd", cmd, str(REPO_ROOT), None, timeout=60)
    finally:
        marker.unlink(missing_ok=True)
    if status != v.PASS_AFTER_RETRY:
        raise AssertionError(
            f"the LNK1104 retry leg reported {status!r}, expected non-certifying PASS-AFTER-RETRY"
        )
    log = (v.LOG_DIR / "probe-status-lnk.log").read_text(encoding="utf-8", errors="replace")
    if "ATTEMPT1" not in log or "ATTEMPT2" not in log:
        raise AssertionError(
            "the retried gate's log dropped an attempt — a repeat LNK1104 flake is invisible if the "
            f"first attempt is discarded. log:\n{log}"
        )
    (v.LOG_DIR / "probe-status-lnk.log").unlink(missing_ok=True)


def main() -> None:
    tests = [
        test_a_wrong_model_on_the_champion_port_is_a_failure_not_a_skip,
        test_an_unparseable_health_reply_is_a_failure_not_a_skip,
        test_the_matching_pin_still_passes_the_probe,
        test_a_server_that_is_down_still_skips,
        test_run_gate_carries_the_probe_status_through,
        test_a_crashing_probe_fails_only_its_own_gate,
        test_the_lnk1104_retry_keeps_both_attempts_in_the_log,
    ]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"PASS: verify-10 probe status policy ({len(tests)} checks)")


if __name__ == "__main__":
    main()
