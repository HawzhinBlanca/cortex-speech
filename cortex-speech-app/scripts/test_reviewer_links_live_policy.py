import contextlib
import hashlib
import http.server
import importlib.util
import io
import json
import os
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

from pilot_focus_contract import contract_for_ids, verify_controlled_pilot_focus

SCRIPT = Path(__file__).with_name("check_reviewer_links_live.py")
RELEASE = SCRIPT.with_name("release_private_production.py")
VERIFY_10 = SCRIPT.parents[2] / "scripts" / "verify_10.py"
TEST_FOCUS_IDS = ("focus-a", "focus-b")
TEST_FOCUS_CONTRACT = contract_for_ids(TEST_FOCUS_IDS)


def write_test_focus(root: Path) -> None:
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "test", "segment_ids": list(TEST_FOCUS_IDS)}),
        encoding="utf-8",
    )


def write_flexible_pool_db(root: Path) -> str:
    pool_id = "123e4567-e89b-42d3-a456-426614174000"
    connection = sqlite3.connect(root / "cortex-speech.db")
    connection.executescript(
        """
        CREATE TABLE review_pool_registry (
            pool_id TEXT NOT NULL,
            focus_segment_count INTEGER NOT NULL,
            focus_sha256 TEXT NOT NULL
        );
        CREATE TABLE review_pool_members (pool_id TEXT NOT NULL, segment_id TEXT NOT NULL);
        """
    )
    connection.execute(
        "INSERT INTO review_pool_registry(pool_id, focus_segment_count, focus_sha256) VALUES (?,?,?)",
        (pool_id, 2, "a" * 64),
    )
    connection.executemany(
        "INSERT INTO review_pool_members(pool_id, segment_id) VALUES (?,?)",
        [(pool_id, "clip-a"), (pool_id, "clip-b")],
    )
    connection.commit()
    connection.close()
    return pool_id


def load_gate():
    spec = importlib.util.spec_from_file_location("reviewer_links_live_policy", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    module.verify_controlled_pilot_focus = lambda root: verify_controlled_pilot_focus(root, TEST_FOCUS_CONTRACT)
    return module


class ReviewerLinksPolicyTests(unittest.TestCase):
    def test_private_production_mode_detects_the_exact_flexible_pool(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            pool_id = write_flexible_pool_db(root)
            self.assertEqual(gate.active_flexible_pool(root), pool_id)
            connection = sqlite3.connect(root / "cortex-speech.db")
            connection.execute("DELETE FROM review_pool_members WHERE segment_id='clip-b'")
            connection.commit()
            connection.close()
            with self.assertRaisesRegex(RuntimeError, "membership is 1/2"):
                gate.active_flexible_pool(root)

    def test_private_production_probe_accepts_flexible_pool_without_legacy_pilot(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            write_flexible_pool_db(root)
            database = root / "cortex-speech.db"
            (root / "couch_session.json").write_text(
                json.dumps(
                    {
                        "reviewers": {"protected-a": "Alle", "protected-r": "Rubar"},
                        "db_path": str(database),
                        "pilot_policy": None,
                    }
                ),
                encoding="utf-8",
            )

            def decrypt(value):
                return {"protected-a": "token-a", "protected-r": "token-r"}[value]

            def probe(_base, path, body=None, **_kwargs):
                self.assertEqual(path, "/api/claim/probe")
                reviewer = {"token-a": "Alle", "token-r": "Rubar"}[json.loads(body)["token"]]
                return 200, None, json.dumps(
                    {
                        "reviewer": reviewer,
                        "pilotPolicy": None,
                        "dbBindingSha256": hashlib.sha256(str(database).encode("utf-8")).hexdigest(),
                        "durableStateMatches": True,
                    }
                ).encode()

            argv = [
                str(SCRIPT),
                "--base-url",
                "https://127.0.0.1:8737",
                "--data-dir",
                raw,
                "--require-private-production",
            ]
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(gate, "dpapi_unprotect", side_effect=decrypt),
                mock.patch.object(gate, "request", side_effect=probe),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(gate.main(), 0)

    def test_funnel_discovery_requires_one_enabled_route_to_the_exact_port(self):
        gate = load_gate()
        status = {
            "Web": {
                "review.example.ts.net:443": {
                    "Handlers": {"/": {"Proxy": "https+insecure://127.0.0.1:8737"}}
                },
                "other.example.ts.net:443": {
                    "Handlers": {"/": {"Proxy": "https+insecure://127.0.0.1:9999"}}
                },
            },
            "AllowFunnel": {"review.example.ts.net:443": True, "other.example.ts.net:443": True},
        }
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(status), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            self.assertEqual(gate.discover_funnel_base(8737), "https://review.example.ts.net:443")

        status["Web"]["review.example.ts.net:443"]["Handlers"]["/api/queue"] = {
            "Proxy": "https+insecure://127.0.0.1:9999"
        }
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(status), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                gate.discover_funnel_base(8737)
        del status["Web"]["review.example.ts.net:443"]["Handlers"]["/api/queue"]

        status["Web"]["review.example.ts.net:443"]["Handlers"]["/"]["Proxy"] = (
            "https+insecure://evil127.0.0.1:8737"
        )
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(status), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                gate.discover_funnel_base(8737)

        malicious = {
            "Web": {
                "review.example.ts.net@evil.example:443": {
                    "Handlers": {"/": {"Proxy": "https+insecure://127.0.0.1:8737"}}
                }
            },
            "AllowFunnel": {"review.example.ts.net@evil.example:443": True},
        }
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(malicious), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                gate.discover_funnel_base(8737)

        status["Web"]["review.example.ts.net:443"]["Handlers"]["/"]["Proxy"] = (
            "https+insecure://127.0.0.1:8737"
        )
        status["AllowFunnel"]["review.example.ts.net:443"] = False
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(status), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                gate.discover_funnel_base(8737)

        status["AllowFunnel"]["review.example.ts.net:443"] = 1
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(status), stderr="")
        with mock.patch.object(gate.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                gate.discover_funnel_base(8737)

    def test_public_transport_keeps_normal_certificate_verification(self):
        gate = load_gate()
        public = gate.transport_context("https://review.example.ts.net")
        local = gate.transport_context("https://127.0.0.1:8737")
        self.assertTrue(public.check_hostname)
        self.assertEqual(public.verify_mode, ssl.CERT_REQUIRED)
        self.assertFalse(local.check_hostname)
        self.assertEqual(local.verify_mode, ssl.CERT_NONE)

    def test_explicit_base_url_is_loopback_only_and_rejected_before_token_decryption(self):
        gate = load_gate()
        self.assertEqual(gate.validate_loopback_base_url("http://127.0.0.1:8737/"), "http://127.0.0.1:8737")
        self.assertEqual(gate.validate_loopback_base_url("https://[::1]:8737"), "https://[::1]:8737")

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "couch_session.json").write_text(
                json.dumps(
                    {
                        "reviewers": {"dpapi:protected": "Rubar"},
                        "db_path": str(root / "cortex-speech.db"),
                    }
                ),
                encoding="utf-8",
            )
            for unsafe in (
                "https://review.example.ts.net",
                "http://example.com",
                "https://localhost:8737",
                "https://localhost.evil.example:8737",
                "https://user@127.0.0.1:8737",
                "https://127.0.0.1:8737/path",
            ):
                argv = [str(SCRIPT), "--base-url", unsafe, "--data-dir", raw, "--require-links"]
                with (
                    mock.patch.object(sys, "argv", argv),
                    mock.patch.object(gate, "dpapi_unprotect") as decrypt,
                    mock.patch.object(gate, "request") as request,
                ):
                    self.assertEqual(gate.main(), 1, unsafe)
                    decrypt.assert_not_called()
                    request.assert_not_called()

    def test_probe_transport_never_uses_inherited_http_proxies(self):
        gate = load_gate()
        response = mock.MagicMock()
        response.status = 200
        response.headers.get.return_value = None
        response.read.return_value = b"{}"
        opener = mock.MagicMock()
        opener.open.return_value = response
        with mock.patch.object(gate.urllib.request, "build_opener", return_value=opener) as build:
            self.assertEqual(
                gate.request("https://127.0.0.1:8737", "/api/claim/probe", b'{"token":"secret"}'),
                (200, None, b"{}"),
            )
        handlers = build.call_args.args
        proxy_handlers = [handler for handler in handlers if isinstance(handler, gate.urllib.request.ProxyHandler)]
        self.assertEqual(len(proxy_handlers), 1)
        self.assertEqual(proxy_handlers[0].proxies, {})

    def test_require_links_makes_an_empty_session_red(self):
        with tempfile.TemporaryDirectory() as raw:
            env = os.environ.copy()
            env["PYTHONUTF8"] = "1"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--base-url",
                    "https://127.0.0.1:8737",
                    "--data-dir",
                    raw,
                    "--require-links",
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                env=env,
                check=False,
            )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("no couch session", completed.stdout)

    def test_pilot_probe_is_exactly_two_links_and_never_claims_or_queues(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            policy = {
                "schema_version": 1,
                "after_review_event_id": 863,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    {"name": "Rubar", "max_corpus_actions": 10},
                    {"name": "Alle", "max_corpus_actions": 10},
                ],
            }
            (root / "review_pilot_policy.json").write_text(json.dumps(policy), encoding="utf-8")
            write_test_focus(root)
            (root / "couch_session.json").write_text(
                json.dumps(
                    {
                        "reviewers": {"protected-h": "Rubar", "protected-p": "Alle"},
                        "db_path": str(root / "cortex-speech.db"),
                        "pilot_policy": policy,
                    }
                ),
                encoding="utf-8",
            )
            calls: list[str] = []

            def probe(_base, path, body=None, **_kwargs):
                calls.append(path)
                token = json.loads(body)["token"]
                reviewer = {"token-h": "Rubar", "token-p": "Alle"}[token]
                return 200, None, json.dumps(
                    {
                        "reviewer": reviewer,
                        "pilotPolicy": policy,
                        "dbBindingSha256": hashlib.sha256(
                            str(root / "cortex-speech.db").encode("utf-8")
                        ).hexdigest(),
                        "durableStateMatches": True,
                    }
                ).encode()

            argv = [
                str(SCRIPT),
                "--base-url",
                "https://127.0.0.1:8737",
                "--data-dir",
                raw,
                "--require-links",
                "--require-pilot",
            ]
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(gate, "dpapi_unprotect", side_effect=["token-p", "token-h"]),
                mock.patch.object(gate, "request", side_effect=probe),
            ):
                self.assertEqual(gate.main(), 0)
            self.assertEqual(calls, ["/api/claim/probe", "/api/claim/probe"])

            # A one-link session must never certify the two-person pilot.
            session = json.loads((root / "couch_session.json").read_text(encoding="utf-8"))
            session["reviewers"].pop("protected-p")
            (root / "couch_session.json").write_text(json.dumps(session), encoding="utf-8")
            with mock.patch.object(sys, "argv", argv):
                self.assertEqual(gate.main(), 1)

    def test_pilot_policy_rejects_bool_unknown_fields_and_old_names(self):
        gate = load_gate()
        valid = {
            "schema_version": 1,
            "after_review_event_id": 863,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Rubar", "max_corpus_actions": 10},
                {"name": "Alle", "max_corpus_actions": 10},
            ],
        }
        for mutation in (
            lambda value: value.update(after_review_event_id=True),
            lambda value: value.update(unknown=True),
            lambda value: value["reviewers"][0].update(unknown=True),
            lambda value: value.update(max_total_review_actions=value.pop("max_total_corpus_actions")),
        ):
            with tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                broken = json.loads(json.dumps(valid))
                mutation(broken)
                (root / "review_pilot_policy.json").write_text(json.dumps(broken), encoding="utf-8")
                with self.assertRaises(RuntimeError):
                    gate.required_pilot_policy(root)

    def test_required_pilot_link_gate_refuses_missing_or_wrong_focus(self):
        gate = load_gate()
        policy = {
            "schema_version": 1,
            "after_review_event_id": 863,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Rubar", "max_corpus_actions": 10},
                {"name": "Alle", "max_corpus_actions": 10},
            ],
        }
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "review_pilot_policy.json").write_text(json.dumps(policy), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "is required"):
                gate.required_pilot_policy(root)
            (root / "voice_focus.json").write_text(
                json.dumps({"segment_ids": ["focus-a", "focus-wrong"]}), encoding="utf-8"
            )
            with self.assertRaisesRegex(RuntimeError, "digest mismatch"):
                gate.required_pilot_policy(root)
            write_test_focus(root)
            self.assertEqual(gate.required_pilot_policy(root)[1], {"Rubar", "Alle"})

    def test_strict_json_rejects_duplicate_fields_and_nonfinite_numbers(self):
        gate = load_gate()
        with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
            gate.strict_json_loads('{"after_review_event_id": 1, "after_review_event_id": 2}')
        with self.assertRaisesRegex(ValueError, "non-finite JSON number"):
            gate.strict_json_loads('{"after_review_event_id": NaN}')

    def test_pilot_refuses_wrong_saved_or_live_database_and_nonproduction_port(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            policy = {
                "schema_version": 1,
                "after_review_event_id": 863,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    {"name": "Rubar", "max_corpus_actions": 10},
                    {"name": "Alle", "max_corpus_actions": 10},
                ],
            }
            (root / "review_pilot_policy.json").write_text(json.dumps(policy), encoding="utf-8")
            write_test_focus(root)
            session = {
                "reviewers": {"protected-h": "Rubar", "protected-p": "Alle"},
                "db_path": str(root / "wrong.db"),
                "pilot_policy": policy,
            }
            session_path = root / "couch_session.json"
            session_path.write_text(json.dumps(session), encoding="utf-8")
            argv = [
                str(SCRIPT), "--base-url", "https://127.0.0.1:8737", "--data-dir", raw,
                "--require-links", "--require-pilot",
            ]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(gate, "request") as request:
                self.assertEqual(gate.main(), 1)
                request.assert_not_called()

            session["db_path"] = str(root / "cortex-speech.db")
            session_path.write_text(json.dumps(session), encoding="utf-8")
            wrong_live = json.dumps(
                {
                    "reviewer": "Rubar",
                    "pilotPolicy": policy,
                    "dbBindingSha256": hashlib.sha256(str(root / "other.db").encode()).hexdigest(),
                    "durableStateMatches": True,
                }
            ).encode()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(gate, "dpapi_unprotect", return_value="token"),
                mock.patch.object(gate, "request", return_value=(200, None, wrong_live)),
            ):
                self.assertEqual(gate.main(), 1)

            with mock.patch.object(sys, "argv", argv + ["--port", "9999"]):
                self.assertEqual(gate.main(), 1)

    def test_saved_session_and_live_policy_types_must_survive_rust_restart(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            policy = {
                "schema_version": 1,
                "after_review_event_id": 863,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    {"name": "Rubar", "max_corpus_actions": 10},
                    {"name": "Alle", "max_corpus_actions": 10},
                ],
            }
            (root / "review_pilot_policy.json").write_text(json.dumps(policy), encoding="utf-8")
            write_test_focus(root)
            session = {
                "reviewers": {"protected-h": "Rubar", "protected-p": "Alle"},
                "db_path": str(root / "cortex-speech.db"),
                "pilot_policy": json.loads(json.dumps(policy)),
            }
            session_path = root / "couch_session.json"
            argv = [
                str(SCRIPT), "--base-url", "https://127.0.0.1:8737", "--data-dir", raw,
                "--require-links", "--require-pilot",
            ]
            for bad_value in (True, 863.0):
                broken = json.loads(json.dumps(session))
                broken["pilot_policy"]["after_review_event_id"] = bad_value
                session_path.write_text(json.dumps(broken), encoding="utf-8")
                with mock.patch.object(sys, "argv", argv), mock.patch.object(gate, "request") as request:
                    self.assertEqual(gate.main(), 1)
                    request.assert_not_called()

            session_path.write_text(json.dumps(session), encoding="utf-8")
            broken_live_policy = json.loads(json.dumps(policy))
            broken_live_policy["schema_version"] = True
            response = {
                "reviewer": "Rubar",
                "pilotPolicy": broken_live_policy,
                "dbBindingSha256": hashlib.sha256(session["db_path"].encode()).hexdigest(),
                "durableStateMatches": True,
            }
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(gate, "dpapi_unprotect", return_value="token"),
                mock.patch.object(gate, "request", return_value=(200, None, json.dumps(response).encode())),
            ):
                self.assertEqual(gate.main(), 1)

    def test_saved_session_nested_shapes_match_rust_deserialization(self):
        gate = load_gate()
        valid = {
            "reviewers": {"protected-h": "Rubar"},
            "db_path": "C:/data/cortex-speech.db",
            "spot_checks": [["segment", "Rubar"]],
            "pilot_spot_checks": [["key", "Rubar"]],
            "sessions": [{"token": "dpapi:AAAA", "reviewer": "Rubar", "issued_unix": 1}],
        }
        gate.validate_saved_session_shape(valid)
        broken_values = [
            {**valid, "spot_checks": [["only-one"]]},
            {**valid, "pilot_spot_checks": [["key", 1]]},
            {**valid, "sessions": [{"token": "x", "reviewer": "Rubar", "issued_unix": True}]},
            {**valid, "sessions": [{"token": "x", "reviewer": "Rubar", "issued_unix": -1}]},
        ]
        for broken in broken_values:
            with self.assertRaises(RuntimeError):
                gate.validate_saved_session_shape(broken)

    def test_untrusted_identity_and_duplicate_credential_key_are_never_logged(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            policy = {
                "schema_version": 1,
                "after_review_event_id": 1,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    {"name": "Rubar", "max_corpus_actions": 10},
                    {"name": "Alle", "max_corpus_actions": 10},
                ],
            }
            (root / "review_pilot_policy.json").write_text(json.dumps(policy), encoding="utf-8")
            write_test_focus(root)
            db_path = str(root / "cortex-speech.db")
            session = {
                "reviewers": {"protected-h": "Rubar", "protected-p": "Alle"},
                "db_path": db_path,
                "pilot_policy": policy,
            }
            session_path = root / "couch_session.json"
            session_path.write_text(json.dumps(session), encoding="utf-8")
            argv = [
                str(SCRIPT), "--base-url", "https://127.0.0.1:8737", "--data-dir", raw,
                "--require-links", "--require-pilot",
            ]
            secret_echo = "plaintext-token\n\x1b[31mINJECT"
            response = {
                "reviewer": secret_echo,
                "pilotPolicy": policy,
                "dbBindingSha256": hashlib.sha256(db_path.encode()).hexdigest(),
                "durableStateMatches": True,
            }
            output = io.StringIO()
            with (
                contextlib.redirect_stdout(output),
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(gate, "dpapi_unprotect", return_value="plaintext-token"),
                mock.patch.object(gate, "request", return_value=(200, None, json.dumps(response).encode())),
            ):
                self.assertEqual(gate.main(), 1)
            self.assertNotIn("plaintext-token", output.getvalue())
            self.assertNotIn("INJECT", output.getvalue())

            malicious_key = "dpapi:SECRET\u001b[31m"
            session_path.write_text(
                '{"reviewers":{"%s":"Rubar","%s":"Alle"},"db_path":"x"}'
                % (malicious_key, malicious_key),
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output), mock.patch.object(sys, "argv", argv):
                self.assertEqual(gate.main(), 1)
            self.assertNotIn("SECRET", output.getvalue())

    def test_dpapi_parser_is_strict_and_forbids_ui(self):
        gate = load_gate()
        for malformed in ("dpapi:QU JD", "dpapi:QUJD!"):
            with self.assertRaises(Exception):
                gate.dpapi_unprotect(malformed)

        class Function:
            def __init__(self):
                self.calls = []

            def __call__(self, *args):
                self.calls.append(args)
                return 0

        crypt = Function()
        local_free = Function()
        crypt32 = type("Crypt32", (), {"CryptUnprotectData": crypt})()
        kernel32 = type("Kernel32", (), {"LocalFree": local_free})()
        # create=True: POSIX ctypes has no WinDLL attribute; the parser logic under test is
        # platform-independent once both DLLs are faked, so it must keep running everywhere.
        with mock.patch.object(gate.ctypes, "WinDLL", side_effect=[crypt32, kernel32], create=True):
            with self.assertRaises(OSError):
                gate.dpapi_unprotect("dpapi:QQ==")
        self.assertEqual(crypt.calls[0][5], 0x1)

    def test_probe_redirect_is_not_followed(self):
        gate = load_gate()
        sink_hits: list[bytes] = []

        class Quiet(http.server.BaseHTTPRequestHandler):
            def log_message(self, _format, *_args):
                pass

        class Sink(Quiet):
            def do_GET(self):
                sink_hits.append(b"GET")
                self.send_response(200)
                self.end_headers()

            def do_POST(self):
                sink_hits.append(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                self.send_response(200)
                self.end_headers()

        sink = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Sink)

        class Redirect(Quiet):
            def do_POST(self):
                self.send_response(302)
                self.send_header("Location", f"http://127.0.0.1:{sink.server_port}/sink")
                self.end_headers()

        redirect = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Redirect)
        ready = [threading.Event(), threading.Event()]
        threads = [
            threading.Thread(
                target=lambda server=server, event=event: (event.set(), server.serve_forever()),
                daemon=True,
            )
            for server, event in zip((sink, redirect), ready, strict=True)
        ]
        for thread in threads:
            thread.start()
        for event in ready:
            self.assertTrue(event.wait(2), "local redirect test server did not start")
        try:
            status, _cookie, _body = gate.request(
                f"http://127.0.0.1:{redirect.server_port}", "/api/claim/probe", b'{"token":"secret"}'
            )
            self.assertEqual(status, 302)
            self.assertEqual(sink_hits, [])
        finally:
            redirect.shutdown()
            sink.shutdown()
            redirect.server_close()
            sink.server_close()

    def test_revocation_metadata_error_is_red(self):
        gate = load_gate()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "couch_session.json").write_text(json.dumps({"reviewers": {"x": "Rubar"}}), encoding="utf-8")
            argv = [str(SCRIPT), "--base-url", "https://127.0.0.1:8737", "--data-dir", raw, "--require-links"]
            original_stat = Path.stat

            def denied(path, *args, **kwargs):
                if path.name == "couch_session.revoked":
                    raise PermissionError("denied")
                return original_stat(path, *args, **kwargs)

            with mock.patch.object(sys, "argv", argv), mock.patch.object(Path, "stat", denied):
                self.assertEqual(gate.main(), 1)

    def test_verify_10_uses_real_funnel_credentials_and_cannot_skip(self):
        spec = importlib.util.spec_from_file_location("verify_10_reviewer_links", VERIFY_10)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        verify = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = verify
        spec.loader.exec_module(verify)

        matches = [entry for entry in verify.GATES if entry[0] == "reviewer-links-live"]
        self.assertEqual(len(matches), 1)
        _name, tier, kind, payload, cwd, probe, _charter = matches[0]
        self.assertEqual((tier, kind, cwd), (2, "cmd", verify.APP))
        self.assertIsNone(probe)
        self.assertIn(str(SCRIPT), payload)
        self.assertIn("--funnel", payload)
        self.assertIn("--port 8737", payload)
        self.assertIn("--require-private-production", payload)
        self.assertNotIn("--require-pilot", payload)

    def test_release_controller_uses_the_same_mode_aware_link_gate(self):
        payload = RELEASE.read_text(encoding="utf-8")
        self.assertIn('"--require-private-production"', payload)


if __name__ == "__main__":
    unittest.main()
