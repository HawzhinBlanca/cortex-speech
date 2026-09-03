#!/usr/bin/env python3
"""Does each reviewer's ACTUAL LINK authenticate through the public route?

`check_supervision_live.py` asks whether the port answers and counts the names in
`couch_session.json`. Both were true, continuously, through nine days in which six of eight paid
reviewers could not get in at all (2026-08-20): their cookies died on every app restart, the page
showed the terminal "link expired", and the gate went on reading OK because it had never presented a
credential to the thing it was grading. A monitor that never authenticates cannot see an
authentication failure.

So this one uses the real credentials over the selected transport, without changing live reviewer state:

  1. the pairing token from `couch_session.json`, decrypted with DPAPI exactly as the server does —
     if this fails the file is not ours and no link in it works;
  2. `POST /api/claim/probe` with that token — the link itself. The endpoint validates the exact same
     durable pairing map as a claim, but is contractually read-only: no cookie is minted, no real phone
     session can be evicted, no clip is leased, and no hidden-check key is consumed;
  3. verify the identity returned by the server is exactly the identity named in the durable session.

Queue eligibility is independently proven by `check_reviewer_queues_live.py`; mixing that check into
this network probe previously changed the very session/lease/hidden-check state it was supposed to
observe. Exit 0 = every required link authenticates without mutation. Exit 1 = at least one does not.

NEVER PRINTS A TOKEN. The credential is the identity; a token in a log or a terminal history is a
reviewer's identity leaked, so only names and outcomes are printed.

Production release run:
  python scripts/check_reviewer_links_live.py --funnel --require-private-production

`--require-private-production` is mode-aware: an active flexible pool requires its immutable pool
registry and forbids a simultaneous legacy pilot policy; without a flexible pool it requires the
exact controlled-pilot contract. `--require-pilot` remains available only for explicit legacy-pilot
diagnostics.
"""

from __future__ import annotations

import argparse
import base64
import ctypes
import ctypes.wintypes as wintypes
import hashlib
import json
import os
import sqlite3
import ssl
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from pilot_focus_contract import verify_controlled_pilot_focus
from review_pilot_hidden_contract import PILOT_REVIEWERS

DPAPI_PREFIX = "dpapi:"


def _reject_duplicate_object_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            # A duplicate can occur inside the reviewer map, whose key is the DPAPI-protected
            # credential. Never include an untrusted/secret JSON key in release-gate output.
            raise ValueError("duplicate JSON object key")
        result[key] = value
    return result


def strict_json_loads(raw: str | bytes) -> object:
    """Match serde's fail-closed object parsing: no duplicate keys or non-finite numbers."""

    def reject_constant(value: str) -> object:
        raise ValueError(f"non-finite JSON number: {value}")

    return json.loads(raw, object_pairs_hook=_reject_duplicate_object_keys, parse_constant=reject_constant)


def canonical_path(path: Path) -> str:
    return os.path.normcase(os.path.normpath(str(path.resolve(strict=False))))


def default_data_dir() -> Path:
    # A production gate cannot be redirected to a clean fixture by an inherited environment variable.
    # Tests/diagnostics use the explicit --data-dir argument so the override is visible in the command.
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def validate_pilot_policy(value: object, source: str) -> tuple[dict[str, object], set[str]]:
    policy = value
    expected = {
        "schema_version": 1,
        "max_total_corpus_actions": 20,
        "reviewers": [
            {"name": PILOT_REVIEWERS[0], "max_corpus_actions": 10},
            {"name": PILOT_REVIEWERS[1], "max_corpus_actions": 10},
        ],
    }
    exact_policy_keys = {"schema_version", "after_review_event_id", "max_total_corpus_actions", "reviewers"}
    if not isinstance(policy, dict) or set(policy) != exact_policy_keys:
        raise RuntimeError(f"{source} fields do not exactly match the controlled-pilot contract")
    if type(policy["schema_version"]) is not int or policy["schema_version"] != expected["schema_version"]:
        raise RuntimeError(f"{source} schema is not exactly integer version 1")
    if (
        type(policy["max_total_corpus_actions"]) is not int
        or policy["max_total_corpus_actions"] != expected["max_total_corpus_actions"]
    ):
        raise RuntimeError(f"{source} is not capped at exactly 20 corpus actions")
    if type(policy["after_review_event_id"]) is not int or policy["after_review_event_id"] < 0:
        raise RuntimeError(f"{source} has no valid integer review-event baseline")
    reviewers = policy.get("reviewers")
    if not isinstance(reviewers, list):
        raise RuntimeError(f"{source} reviewer roster is invalid")
    if len(reviewers) != 2 or not all(isinstance(entry, dict) for entry in reviewers):
        raise RuntimeError(f"{source} must contain exactly two reviewer objects")
    if not all(set(entry) == {"name", "max_corpus_actions"} for entry in reviewers):
        raise RuntimeError(f"{source} reviewer fields do not exactly match the server contract")
    if not all(isinstance(entry["name"], str) and type(entry["max_corpus_actions"]) is int for entry in reviewers):
        raise RuntimeError(f"{source} reviewer values have invalid types")
    normalized = sorted((entry["name"].strip().lower(), entry["max_corpus_actions"]) for entry in reviewers)
    expected_reviewers = sorted((name.lower(), 10) for name in PILOT_REVIEWERS)
    if normalized != expected_reviewers:
        raise RuntimeError(
            f"{source} must contain exactly {' and '.join(PILOT_REVIEWERS)} at 10 corpus actions each"
        )
    return policy, set(PILOT_REVIEWERS)


def required_pilot_policy(root: Path) -> tuple[dict[str, object], set[str]]:
    path = root / "review_pilot_policy.json"
    try:
        policy = strict_json_loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RuntimeError(f"controlled pilot policy cannot be read: {error}") from error
    validated = validate_pilot_policy(policy, "controlled pilot policy")
    verify_controlled_pilot_focus(root)
    return validated


def active_flexible_pool(root: Path) -> str | None:
    """Return the exact active pool id, or None when this is a pre-pool legacy database."""

    database = (root / "cortex-speech.db").resolve(strict=True)
    connection = sqlite3.connect(f"{database.as_uri()}?mode=ro", uri=True)
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN DEFERRED")
        tables = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('review_pool_registry','review_pool_members')"
            )
        }
        if not tables:
            return None
        if tables != {"review_pool_registry", "review_pool_members"}:
            raise RuntimeError(f"flexible review-pool schema is partial: {sorted(tables)}")
        registry = connection.execute(
            "SELECT pool_id, focus_segment_count, focus_sha256 FROM review_pool_registry"
        ).fetchall()
        if not registry:
            member_count = connection.execute("SELECT COUNT(*) FROM review_pool_members").fetchone()[0]
            if member_count:
                raise RuntimeError("flexible review-pool membership exists without its registry")
            return None
        if len(registry) != 1:
            raise RuntimeError(f"flexible review pool has {len(registry)} registry rows")
        pool_id, expected_count, focus_sha256 = registry[0]
        member_count = connection.execute(
            "SELECT COUNT(*) FROM review_pool_members WHERE pool_id=?", (pool_id,)
        ).fetchone()[0]
        if type(expected_count) is not int or expected_count <= 0 or member_count != expected_count:
            raise RuntimeError(
                f"flexible review-pool membership is {member_count}/{expected_count}; authority is incomplete"
            )
        if (
            not isinstance(pool_id, str)
            or len(pool_id) != 36
            or not isinstance(focus_sha256, str)
            or len(focus_sha256) != 64
            or any(char not in "0123456789abcdef" for char in focus_sha256)
        ):
            raise RuntimeError("flexible review-pool registry identity is invalid")
        return pool_id
    except sqlite3.Error as error:
        raise RuntimeError(f"flexible review-pool authority cannot be read: {error}") from error
    finally:
        connection.close()


def validate_saved_session_shape(payload: dict[str, object]) -> None:
    reviewers = payload.get("reviewers")
    if not isinstance(reviewers, dict) or not all(
        isinstance(key, str) and isinstance(value, str) for key, value in reviewers.items()
    ):
        raise RuntimeError("remembered reviewer-link map cannot survive restart")
    if not isinstance(payload.get("db_path"), str):
        raise RuntimeError("remembered database binding cannot survive restart")
    for field in ("spot_checks", "pilot_spot_checks"):
        checks = payload.get(field, [])
        if not isinstance(checks, list) or not all(
            isinstance(entry, list)
            and len(entry) == 2
            and all(isinstance(item, str) for item in entry)
            for entry in checks
        ):
            raise RuntimeError(f"remembered {field} cannot survive restart")
    sessions = payload.get("sessions", [])
    if not isinstance(sessions, list):
        raise RuntimeError("remembered cookie sessions cannot survive restart")
    for entry in sessions:
        if not isinstance(entry, dict):
            raise RuntimeError("remembered cookie session cannot survive restart")
        token = entry.get("token")
        reviewer = entry.get("reviewer")
        issued = entry.get("issued_unix")
        if (
            not isinstance(token, str)
            or not isinstance(reviewer, str)
            or type(issued) is not int
            or issued < 0
            or issued > (2**64 - 1)
        ):
            raise RuntimeError("remembered cookie session cannot survive restart")


class _Blob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def dpapi_unprotect(stored: str) -> str:
    """Mirror of `dpapi::unprotect`. Windows-only, like the store it reads."""
    if not stored.startswith(DPAPI_PREFIX):
        raise ValueError("not a dpapi-protected value")
    raw = base64.b64decode(stored[len(DPAPI_PREFIX) :], validate=True)
    if not raw:
        raise ValueError("empty dpapi payload")
    src = _Blob(len(raw), ctypes.cast(ctypes.create_string_buffer(raw, len(raw)), ctypes.POINTER(ctypes.c_char)))
    out = _Blob()
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt32.CryptUnprotectData.argtypes = [
        ctypes.POINTER(_Blob),
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(_Blob),
    ]
    crypt32.CryptUnprotectData.restype = wintypes.BOOL
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    if not crypt32.CryptUnprotectData(ctypes.byref(src), None, None, None, None, 0x1, ctypes.byref(out)):
        raise OSError("CryptUnprotectData failed — this session file belongs to another user or machine")
    try:
        return ctypes.string_at(out.pbData, out.cbData).decode("utf-8")
    finally:
        kernel32.LocalFree(ctypes.cast(out.pbData, ctypes.c_void_p))


def _ctx() -> ssl.SSLContext:
    # The couch server presents its own self-signed identity (couch_tls_identity.json); this probe is
    # talking to 127.0.0.1 on the same machine that minted it, so pinning nothing is correct here.
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def transport_context(base: str) -> ssl.SSLContext:
    """Trust public Funnel TLS normally; relax verification only for the loopback self-signed server."""
    host = (urllib.parse.urlparse(base).hostname or "").lower()
    if host in {"127.0.0.1", "localhost", "::1"}:
        return _ctx()
    return ssl.create_default_context()


def validate_loopback_base_url(raw: str) -> str:
    """Return a canonical explicit diagnostic origin, rejecting every non-loopback target."""
    try:
        parsed = urllib.parse.urlparse(raw)
        port = parsed.port
    except (TypeError, ValueError) as error:
        raise RuntimeError("--base-url must be an explicit HTTP(S) loopback origin") from error

    host = (parsed.hostname or "").lower()
    if (
        raw != raw.strip()
        or any(ord(char) < 0x20 or ord(char) == 0x7F for char in raw)
        or parsed.scheme.lower() not in {"http", "https"}
        # Numeric literals avoid DNS/hosts-file rebinding of a name that merely looks local.
        or host not in {"127.0.0.1", "::1"}
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in {"", "/"}
        or parsed.params
        or parsed.query
        or parsed.fragment
    ):
        raise RuntimeError("--base-url must be an explicit HTTP(S) loopback origin")

    host_literal = f"[{host}]" if ":" in host else host
    port_suffix = f":{port}" if port is not None else ""
    return f"{parsed.scheme.lower()}://{host_literal}{port_suffix}"


def discover_funnel_base(port: int) -> str:
    """Resolve the one advertised Funnel HTTPS route that proxies to this Couch port."""
    completed = subprocess.run(
        ["tailscale", "funnel", "status", "--json"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=20,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"tailscale funnel status failed: {(completed.stderr or completed.stdout).strip()}")
    try:
        payload = strict_json_loads(completed.stdout)
    except ValueError as error:
        raise RuntimeError(f"tailscale funnel status was not JSON: {error}") from error

    allowed = payload.get("AllowFunnel") or {}
    matches: list[str] = []
    for authority, route in (payload.get("Web") or {}).items():
        # Tailscale uses a handler map with longest-path routing. Proving only `/` is unsafe: a
        # more-specific `/api/...` entry can send the real reviewer traffic somewhere else while
        # the root claim probe still greens. This pilot has one service, so accept exactly one root
        # handler and require the JSON boolean `true` (not truthy 1/string data).
        if allowed.get(authority) is not True or not isinstance(route, dict):
            continue
        handlers = route.get("Handlers")
        if not isinstance(handlers, dict) or set(handlers) != {"/"}:
            continue
        root_handler = handlers.get("/")
        if not isinstance(root_handler, dict):
            continue
        public = urllib.parse.urlparse(f"https://{authority}")
        public_host = (public.hostname or "").lower()
        if (
            public.scheme != "https"
            or not public_host.endswith(".ts.net")
            or public.port not in {None, 443}
            or public.username is not None
            or public.password is not None
            or public.path not in {"", "/"}
            or public.params
            or public.query
            or public.fragment
            or public.netloc != authority
        ):
            continue
        proxy = str(root_handler.get("Proxy") or "")
        parsed = urllib.parse.urlparse(proxy)
        if (
            parsed.scheme in {"http", "https", "https+insecure"}
            and (parsed.hostname or "").lower() in {"127.0.0.1", "localhost", "::1"}
            and parsed.port == port
            and parsed.username is None
            and parsed.password is None
            and parsed.path in {"", "/"}
            and not parsed.params
            and not parsed.query
            and not parsed.fragment
        ):
            matches.append(public.geturl().rstrip("/"))
    if len(matches) != 1:
        raise RuntimeError(
            f"expected exactly one enabled Funnel route to Couch port {port}, found {len(matches)}"
        )
    return matches[0]


def request(
    base: str,
    path: str,
    body: bytes | None = None,
    cookie: str | None = None,
    context: ssl.SSLContext | None = None,
):
    req = urllib.request.Request(base + path, data=body, method="POST" if body else "GET")
    if body:
        req.add_header("Content-Type", "application/json")
    if cookie:
        req.add_header("Cookie", cookie)
    try:
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001, ARG002
                return None

        opener = urllib.request.build_opener(
            # Reviewer credentials must travel directly to the validated origin, never through an
            # inherited HTTP(S)_PROXY that could observe or redirect a loopback diagnostic request.
            urllib.request.ProxyHandler({}),
            NoRedirect(),
            urllib.request.HTTPSHandler(context=context or transport_context(base)),
        )
        resp = opener.open(req, timeout=20)
        return resp.status, resp.headers.get("Set-Cookie"), resp.read()
    except urllib.error.HTTPError as e:
        # The STATUS is the answer here; the body is best-effort diagnostics. Reading it can still
        # fail after the response line and headers arrived intact: a peer that replies without
        # draining our request body makes the close send an RST on BSD stacks, so this raises
        # ConnectionResetError on macOS where Linux and Windows return b"". Letting that escape
        # crashed the whole reviewer-links gate over a status code it already held -- and this gate
        # is what says whether reviewers can work. Never lose a status to a body read.
        try:
            body = e.read()
        except Exception as read_error:  # noqa: BLE001 - an unreadable body is not a missing status
            body = f"<body unreadable: {read_error}>".encode()
        return e.code, None, body
    except Exception as e:  # noqa: BLE001 - transport failure is a real answer here
        return None, None, str(e).encode()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8737)
    parser.add_argument("--data-dir", type=Path, default=default_data_dir())
    transport = parser.add_mutually_exclusive_group()
    transport.add_argument("--funnel", action="store_true", help="use the enabled Tailscale Funnel route")
    transport.add_argument(
        "--base-url",
        help="explicit HTTP(S) loopback origin (test/diagnostic only); public routes require --funnel",
    )
    parser.add_argument(
        "--require-links",
        action="store_true",
        help="fail when no active reviewer links exist; required by the production release gate",
    )
    parser.add_argument(
        "--require-pilot",
        action="store_true",
        help=f"legacy only: require the exact {'/'.join(PILOT_REVIEWERS)} controlled-pilot policy and link roster",
    )
    parser.add_argument(
        "--require-private-production",
        action="store_true",
        help="require the active flexible-pool mode, or the exact legacy pilot when no pool exists",
    )
    args = parser.parse_args()
    root = args.data_dir.resolve()
    if args.require_pilot and args.require_private_production:
        print("REVIEWER LINKS: FAIL — choose mode-aware private production or explicit legacy pilot, not both")
        return 1
    if args.require_private_production:
        args.require_links = True
    if (args.require_pilot or args.require_private_production) and args.port != 8737:
        print("REVIEWER LINKS: FAIL — private production must use Couch port 8737")
        return 1
    try:
        base = (
            discover_funnel_base(args.port)
            if args.funnel
            else validate_loopback_base_url(args.base_url or f"https://127.0.0.1:{args.port}")
        )
    except Exception as error:  # noqa: BLE001 - an undiscoverable advertised route is operationally red
        print(f"REVIEWER LINKS: FAIL — public transport cannot be resolved ({error})")
        return 1
    context = transport_context(base)

    print("REVIEWER LINKS (live, with real credentials)")
    revocation_file = root / "couch_session.revoked"
    try:
        revocation_file.stat()
        revoked = True
    except FileNotFoundError:
        revoked = False
    except OSError as error:
        print(f"REVIEWER LINKS: FAIL — revocation state cannot be verified ({error})")
        return 1
    if revoked:
        if args.require_links:
            print("REVIEWER LINKS: FAIL — the couch session is revoked, so every paid-review link is dead")
            return 1
        print("REVIEWER LINKS: OK (session revoked by Stop — every link is intentionally dead)")
        return 0

    session_file = root / "couch_session.json"
    if not session_file.is_file():
        if args.require_links:
            print("REVIEWER LINKS: FAIL — no couch session exists, so no paid reviewer can work")
            return 1
        print("REVIEWER LINKS: OK (no couch session — no links have been issued)")
        return 0
    try:
        payload = strict_json_loads(session_file.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        print(f"REVIEWER LINKS: FAIL — couch_session.json cannot be read ({e})")
        return 1
    if not isinstance(payload, dict):
        print("REVIEWER LINKS: FAIL — couch_session.json is not a JSON object")
        return 1
    try:
        validate_saved_session_shape(payload)
    except RuntimeError as error:
        print(f"REVIEWER LINKS: FAIL — {error}")
        return 1
    links = payload.get("reviewers") or {}
    if not links:
        if args.require_links:
            print("REVIEWER LINKS: FAIL — the couch session contains no reviewer links")
            return 1
        print("REVIEWER LINKS: OK (session file names no links)")
        return 0

    if not isinstance(links, dict) or not all(isinstance(key, str) and isinstance(value, str) for key, value in links.items()):
        print("REVIEWER LINKS: FAIL — the durable reviewer-link map is invalid")
        return 1
    remembered_policy: dict[str, object] | None = None
    flexible_pool_id: str | None = None
    require_pilot = args.require_pilot
    if args.require_private_production:
        try:
            flexible_pool_id = active_flexible_pool(root)
        except (OSError, RuntimeError) as error:
            print(f"REVIEWER LINKS: FAIL — {error}")
            return 1
        require_pilot = flexible_pool_id is None
        expected_db = root / "cortex-speech.db"
        recorded_db = payload.get("db_path")
        if not isinstance(recorded_db, str) or canonical_path(Path(recorded_db)) != canonical_path(expected_db):
            print("REVIEWER LINKS: FAIL — the remembered session is bound to a different database")
            return 1
        actual_reviewers = [name.strip().lower() for name in links.values()]
        if len(set(actual_reviewers)) != len(actual_reviewers):
            print("REVIEWER LINKS: FAIL — the durable session repeats one reviewer identity")
            return 1
        if flexible_pool_id is not None:
            if payload.get("pilot_policy") is not None or (root / "review_pilot_policy.json").exists():
                print("REVIEWER LINKS: FAIL — flexible pool and legacy controlled-pilot policy are active together")
                return 1
    if require_pilot:
        try:
            policy, required_reviewers = required_pilot_policy(root)
        except RuntimeError as error:
            print(f"REVIEWER LINKS: FAIL — {error}")
            return 1
        try:
            remembered_policy, _ = validate_pilot_policy(payload.get("pilot_policy"), "remembered pilot policy")
        except RuntimeError as error:
            print(f"REVIEWER LINKS: FAIL — {error}")
            return 1
        if remembered_policy != policy:
            print("REVIEWER LINKS: FAIL — the remembered session is not bound to the active pilot policy")
            return 1
        recorded_db = payload.get("db_path")
        expected_db = root / "cortex-speech.db"
        if not isinstance(recorded_db, str) or canonical_path(Path(recorded_db)) != canonical_path(expected_db):
            print("REVIEWER LINKS: FAIL — the remembered session is bound to a different database")
            return 1
        actual_reviewers = list(links.values())
        normalized = {name.strip().lower() for name in actual_reviewers}
        if len(actual_reviewers) != 2 or normalized != {name.lower() for name in required_reviewers}:
            print(
                "REVIEWER LINKS: FAIL — the durable links are not exactly "
                + " and ".join(PILOT_REVIEWERS)
            )
            return 1

    failures: list[str] = []
    rows: list[tuple[str, str]] = []
    for protected, name in sorted(links.items(), key=lambda kv: kv[1].lower()):
        try:
            token = dpapi_unprotect(protected)
        except Exception as e:  # noqa: BLE001
            failures.append(f"{name}: their token could not be decrypted ({e})")
            rows.append((name, "TOKEN UNREADABLE"))
            continue

        status, set_cookie, body = request(
            base, "/api/claim/probe", json.dumps({"token": token}).encode(), context=context
        )
        del token  # done with the credential; do not keep it alive longer than the call
        if status is None:
            failures.append(f"{name}: nothing answered on {base} — the server is down, every link is dead")
            rows.append((name, "SERVER DOWN"))
            continue
        if status != 200:
            failures.append(f"{name}: their link was REFUSED by the server (probe returned {status})")
            rows.append((name, f"LINK REFUSED ({status})"))
            continue
        if set_cookie:
            failures.append(f"{name}: the read-only probe unexpectedly minted a cookie")
            rows.append((name, "PROBE MUTATED SESSION"))
            continue
        try:
            probe = strict_json_loads(body)
        except ValueError:
            failures.append(f"{name}: the credential probe returned something that is not JSON")
            rows.append((name, "PROBE UNPARSEABLE"))
            continue
        if not isinstance(probe, dict) or set(probe) != {
            "reviewer",
            "pilotPolicy",
            "dbBindingSha256",
            "durableStateMatches",
        }:
            failures.append(f"{name}: the credential probe returned fields outside the read-only contract")
            rows.append((name, "PROBE CONTRACT MISMATCH"))
            continue
        who = probe.get("reviewer")
        if who != name:
            # Never print the untrusted returned identity. A broken endpoint could echo the submitted
            # pairing token here; logging it would leak the credential this gate promises to protect.
            failures.append(f"{name}: the server bound this link to the wrong identity")
            rows.append((name, "WRONG IDENTITY"))
            continue
        if require_pilot:
            try:
                live_policy, _ = validate_pilot_policy(probe.get("pilotPolicy"), "running pilot policy")
            except RuntimeError as error:
                failures.append(f"{name}: {error}")
                rows.append((name, "WRONG LIVE PILOT POLICY"))
                continue
            if live_policy != remembered_policy:
                failures.append(f"{name}: the running server is bound to a different pilot policy")
                rows.append((name, "WRONG LIVE PILOT POLICY"))
                continue
        elif flexible_pool_id is not None and probe.get("pilotPolicy") is not None:
            failures.append(f"{name}: the running server still exposes a legacy pilot policy in flexible-pool mode")
            rows.append((name, "WRONG LIVE REVIEW MODE"))
            continue
        live_db_binding = probe.get("dbBindingSha256")
        expected_db_binding = hashlib.sha256(payload["db_path"].encode("utf-8")).hexdigest()
        if live_db_binding != expected_db_binding:
            failures.append(f"{name}: the running server is bound to a different database")
            rows.append((name, "WRONG LIVE DATABASE"))
            continue
        if probe.get("durableStateMatches") is not True:
            failures.append(f"{name}: the running reviewer state is not durably restart-safe")
            rows.append((name, "LIVE STATE NOT DURABLE"))
            continue
        rows.append((name, "AUTHENTICATES (read-only)"))

    width = max(len(name) for name, _ in rows)
    for name, outcome in rows:
        mark = "  " if not outcome.startswith(("LINK REFUSED", "SERVER", "TOKEN", "PROBE", "WRONG")) else "! "
        print(f"{mark}{name.ljust(width)}  {outcome}")

    if failures:
        print(f"REVIEWER LINKS: FAIL — {len(failures)} reviewer(s) cannot work")
        for f in failures:
            print(f"  - {f}")
        return 1
    print(f"REVIEWER LINKS: OK — all {len(rows)} required link(s) authenticate without changing live state")
    return 0


if __name__ == "__main__":
    sys.exit(main())
