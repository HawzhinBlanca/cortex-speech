#!/usr/bin/env python3
"""Does each reviewer's ACTUAL LINK let them in, and does it hand them work?

`check_supervision_live.py` asks whether the port answers and counts the names in
`couch_session.json`. Both were true, continuously, through nine days in which six of eight paid
reviewers could not get in at all (2026-08-20): their cookies died on every app restart, the page
showed the terminal "link expired", and the gate went on reading OK because it had never presented a
credential to the thing it was grading. A monitor that never authenticates cannot see an
authentication failure.

So this one uses the real credentials, over the real transport, and reports what each reviewer would
actually receive:

  1. the pairing token from `couch_session.json`, decrypted with DPAPI exactly as the server does —
     if this fails the file is not ours and no link in it works;
  2. `POST /api/claim` with that token — the link itself. 401 here means the link the owner sent is
     dead and no amount of reloading will fix it;
  3. `GET /api/queue` with the cookie the claim returned — the reviewer's real queue. Reports how
     many clips they were handed, and DISTINGUISHES the two ways of getting zero: a dialect/focus
     restriction that leaves them nothing to do, versus a genuinely finished queue.

Exit 0 = every live link authenticates. Exit 1 = at least one reviewer cannot get in, or is admitted
to an empty queue while being paid.

NEVER PRINTS A TOKEN. The credential is the identity; a token in a log or a terminal history is a
reviewer's identity leaked, so only names and outcomes are printed.

One caveat by design: claiming is the only way to prove a link works, and each claim mints a session.
The server keeps at most MAX_SESSIONS_PER_REVIEWER (8) per reviewer and evicts the OLDEST when it has
to, so running this in a tight loop could evict a real device's session. Run it after a deploy, or
when a reviewer reports trouble — not on a timer.

Run:  python scripts/check_reviewer_links_live.py [--port 8737]
"""

from __future__ import annotations

import argparse
import base64
import ctypes
import ctypes.wintypes as wintypes
import json
import os
import ssl
import sys
import urllib.error
import urllib.request
from pathlib import Path

DPAPI_PREFIX = "dpapi:"


def data_dir() -> Path:
    override = os.environ.get("CORTEX_DATA_DIR")
    if override:
        return Path(override)
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


class _Blob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def dpapi_unprotect(stored: str) -> str:
    """Mirror of `dpapi::unprotect`. Windows-only, like the store it reads."""
    if not stored.startswith(DPAPI_PREFIX):
        raise ValueError("not a dpapi-protected value")
    raw = base64.b64decode(stored[len(DPAPI_PREFIX) :])
    src = _Blob(len(raw), ctypes.cast(ctypes.create_string_buffer(raw, len(raw)), ctypes.POINTER(ctypes.c_char)))
    out = _Blob()
    if not ctypes.windll.crypt32.CryptUnprotectData(
        ctypes.byref(src), None, None, None, None, 0, ctypes.byref(out)
    ):
        raise OSError("CryptUnprotectData failed — this session file belongs to another user or machine")
    try:
        return ctypes.string_at(out.pbData, out.cbData).decode("utf-8")
    finally:
        ctypes.windll.kernel32.LocalFree(out.pbData)


def _ctx() -> ssl.SSLContext:
    # The couch server presents its own self-signed identity (couch_tls_identity.json); this probe is
    # talking to 127.0.0.1 on the same machine that minted it, so pinning nothing is correct here.
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def request(base: str, path: str, body: bytes | None = None, cookie: str | None = None):
    req = urllib.request.Request(base + path, data=body, method="POST" if body else "GET")
    if body:
        req.add_header("Content-Type", "application/json")
    if cookie:
        req.add_header("Cookie", cookie)
    try:
        resp = urllib.request.urlopen(req, context=_ctx(), timeout=20)
        return resp.status, resp.headers.get("Set-Cookie"), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, None, e.read()
    except Exception as e:  # noqa: BLE001 - transport failure is a real answer here
        return None, None, str(e).encode()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=int(os.environ.get("CORTEX_COUCH_PORT", "8737")))
    args = parser.parse_args()
    base = f"https://127.0.0.1:{args.port}"

    print("REVIEWER LINKS (live, with real credentials)")
    session_file = data_dir() / "couch_session.json"
    if not session_file.is_file():
        print("REVIEWER LINKS: OK (no couch session — no links have been issued)")
        return 0
    try:
        payload = json.loads(session_file.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        print(f"REVIEWER LINKS: FAIL — couch_session.json cannot be read ({e})")
        return 1
    if (data_dir() / "couch_session.revoked").exists():
        print("REVIEWER LINKS: OK (session revoked by Stop — every link is intentionally dead)")
        return 0

    links = payload.get("reviewers") or {}
    if not links:
        print("REVIEWER LINKS: OK (session file names no links)")
        return 0

    failures: list[str] = []
    idle: list[str] = []
    rows: list[tuple[str, str]] = []
    for protected, name in sorted(links.items(), key=lambda kv: kv[1].lower()):
        try:
            token = dpapi_unprotect(protected)
        except Exception as e:  # noqa: BLE001
            failures.append(f"{name}: their token could not be decrypted ({e})")
            rows.append((name, "TOKEN UNREADABLE"))
            continue

        status, set_cookie, body = request(base, "/api/claim", json.dumps({"token": token}).encode())
        del token  # done with the credential; do not keep it alive longer than the call
        if status is None:
            failures.append(f"{name}: nothing answered on {base} — the server is down, every link is dead")
            rows.append((name, "SERVER DOWN"))
            continue
        if status != 200 or not set_cookie:
            failures.append(f"{name}: their link was REFUSED by the server (claim returned {status})")
            rows.append((name, f"LINK REFUSED ({status})"))
            continue

        cookie = set_cookie.split(";", 1)[0]
        qstatus, _, qbody = request(base, "/api/queue", cookie=cookie)
        if qstatus != 200:
            failures.append(f"{name}: claimed a session but the queue refused it ({qstatus})")
            rows.append((name, f"QUEUE {qstatus}"))
            continue
        try:
            queue = json.loads(qbody)
        except ValueError:
            failures.append(f"{name}: the queue returned something that is not JSON")
            rows.append((name, "QUEUE UNPARSEABLE"))
            continue

        served = len(queue.get("items") or [])
        pending = queue.get("pendingTotal")
        who = queue.get("reviewer")
        if who != name:
            failures.append(f"{name}: the server answered as {who!r} — a link is bound to the wrong identity")
            rows.append((name, f"WRONG IDENTITY ({who})"))
            continue
        if served == 0:
            # The two ways to be handed nothing are not the same problem, and the page already knows
            # the difference — mirror it rather than reporting one number for both.
            if queue.get("noWorkInYourDialect"):
                idle.append(f"{name}: admitted, but has NO clips they may judge (dialect and/or voice focus)")
                rows.append((name, "IN, but 0 clips (restricted)"))
            else:
                rows.append((name, "IN, 0 clips (queue finished)"))
        else:
            rows.append((name, f"IN, {served} clip(s) served, {pending} pending"))

    width = max(len(name) for name, _ in rows)
    for name, outcome in rows:
        mark = "  " if not outcome.startswith(("LINK", "SERVER", "TOKEN", "QUEUE", "WRONG")) else "! "
        print(f"{mark}{name.ljust(width)}  {outcome}")

    if failures:
        print(f"REVIEWER LINKS: FAIL — {len(failures)} reviewer(s) cannot work")
        for f in failures:
            print(f"  - {f}")
        return 1
    if idle:
        print(f"REVIEWER LINKS: FAIL — every link authenticates, but {len(idle)} reviewer(s) have nothing to do")
        for i in idle:
            print(f"  - {i}")
        print("    They are being paid to open an empty queue. Import audio in their dialect, widen")
        print("    the voice focus, or pause their links until there is work for them.")
        return 1
    print(f"REVIEWER LINKS: OK — all {len(rows)} link(s) authenticate and serve work")
    return 0


if __name__ == "__main__":
    sys.exit(main())
