#!/usr/bin/env python3
"""Identity-contract tests for cortex_7b_client.py (no DB, WSL, or model required)."""

import contextlib
import importlib.util
import io
import json
import socket
import sys
import tempfile
import threading
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "cortex_7b_identity_client", Path(__file__).parent / "cortex_7b_client.py"
)
_client = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_client)


def _identity_response(**extra):
    value = {
        "schema": 1,
        "status": "ready",
        "protocol": "cortex-omniasr-adapter",
        "protocolVersion": 1,
        "family": "omniasr-7b",
        "modelVersionId": "omniasr-7b-test@0123456789ab",
        "deploymentSha256": "1" * 64,
        "manifestSha256": "2" * 64,
        "componentSha256": {
            "base": "3" * 64,
            "adapter": "4" * 64,
            "adapterConfig": "5" * 64,
            "tokenizer": "6" * 64,
        },
        "language": "ckb_Arab",
        "provenanceKind": "flywheel",
        "worker": "gpu0",
    }
    value.update(extra)
    return value


def _write_pointer(path: Path, *, model_id: str, deployment_sha: str) -> None:
    path.write_text(
        json.dumps(
            {
                "schema": 2,
                "champions": {
                    "omniasr-7b": {
                        "modelVersionId": model_id,
                        "deploymentManifestPath": "/models/deployment_manifest.json",
                        "deploymentSha256": deployment_sha,
                        "source": "test",
                        "license": "test",
                    }
                },
            }
        ),
        encoding="utf-8",
    )


def test_health_mode_never_resolves_the_database() -> None:
    original_request = _client.request_server
    original_resolve = _client.resolve_db_path
    original_argv = sys.argv
    try:
        _client.request_server = lambda request, timeout: (
            _identity_response() if request == {"op": "health"} and timeout == _client.HEALTH_TIMEOUT_SECONDS else None
        )

        def forbidden_db_lookup():
            raise AssertionError("--health touched the database")

        _client.resolve_db_path = forbidden_db_lookup
        sys.argv = ["cortex_7b_client.py", "--health", "--stdout-only"]
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            _client.main()
        line = output.getvalue().strip()
        assert line.startswith("__HEALTH__=")
        parsed = json.loads(line.removeprefix("__HEALTH__="))
        assert parsed["modelVersionId"] == "omniasr-7b-test@0123456789ab"
        assert parsed["deploymentSha256"] == "1" * 64
    finally:
        _client.request_server = original_request
        _client.resolve_db_path = original_resolve
        sys.argv = original_argv


def test_health_mode_is_bound_to_the_exact_current_pointer() -> None:
    """A structurally valid stale champion must never produce a READY marker."""
    original_request = _client.request_server
    original_argv = sys.argv
    try:
        with tempfile.TemporaryDirectory() as name:
            pointer = Path(name) / "champion.json"
            _write_pointer(
                pointer,
                model_id="omniasr-7b-current@abcdef",
                deployment_sha="a" * 64,
            )
            _client.request_server = lambda request, timeout: _identity_response(
                modelVersionId="omniasr-7b-stale@012345",
                deploymentSha256="b" * 64,
            )
            sys.argv = [
                "cortex_7b_client.py",
                "--health",
                "--expected-pointer",
                str(pointer),
            ]
            stdout = io.StringIO()
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    _client.main()
            except SystemExit as exc:
                assert exc.code == _client.EX_SERVER
            else:
                raise AssertionError("stale valid champion was accepted as current")
            assert "__HEALTH__=" not in stdout.getvalue()
            assert "does not match the current champion pointer" in stderr.getvalue()

            matching = _identity_response(
                modelVersionId="omniasr-7b-current@abcdef",
                deploymentSha256="a" * 64,
            )
            _client.request_server = lambda request, timeout: matching
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                _client.main()
            assert json.loads(stdout.getvalue().strip().removeprefix("__HEALTH__=")) == matching
    finally:
        _client.request_server = original_request
        sys.argv = original_argv


def test_well_formed_stale_loopback_listener_cannot_claim_ready() -> None:
    """Exercise the former false-READY path through a real bounded TCP exchange."""
    original_host = _client.HOST
    original_port = _client.PORT
    original_argv = sys.argv
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    port = listener.getsockname()[1]
    server_error = []

    def serve_stale_identity() -> None:
        try:
            connection, _address = listener.accept()
            with connection:
                request = bytearray()
                while b"\n" not in request:
                    chunk = connection.recv(4096)
                    if not chunk:
                        break
                    request.extend(chunk)
                assert json.loads(bytes(request).decode("utf-8")) == {"op": "health"}
                stale = _identity_response(
                    modelVersionId="omniasr-7b-stale@012345",
                    deploymentSha256="b" * 64,
                )
                connection.sendall((json.dumps(stale) + "\n").encode("utf-8"))
        except Exception as exc:  # surfaced in the main test thread below
            server_error.append(exc)

    worker = threading.Thread(target=serve_stale_identity, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory() as name:
            pointer = Path(name) / "champion.json"
            _write_pointer(
                pointer,
                model_id="omniasr-7b-current@abcdef",
                deployment_sha="a" * 64,
            )
            _client.HOST = "127.0.0.1"
            _client.PORT = port
            sys.argv = [
                "cortex_7b_client.py",
                "--health",
                "--expected-pointer",
                str(pointer),
            ]
            stdout = io.StringIO()
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    _client.main()
            except SystemExit as exc:
                assert exc.code == _client.EX_SERVER
            else:
                raise AssertionError("stale loopback listener produced READY")
            assert "__HEALTH__=" not in stdout.getvalue()
            assert "does not match the current champion pointer" in stderr.getvalue()
    finally:
        listener.close()
        worker.join(timeout=2)
        _client.HOST = original_host
        _client.PORT = original_port
        sys.argv = original_argv
    assert not worker.is_alive(), "fake loopback listener did not terminate"
    assert not server_error, server_error


def test_expected_pointer_is_bounded_and_duplicate_key_safe() -> None:
    with tempfile.TemporaryDirectory() as name:
        pointer = Path(name) / "champion.json"
        _write_pointer(
            pointer,
            model_id="model",
            deployment_sha="a" * 64,
        )
        ambiguous = pointer.read_text(encoding="utf-8").replace(
            '"schema": 2', '"schema": 2, "schema": 2', 1
        )
        pointer.write_text(ambiguous, encoding="utf-8")
        try:
            _client.load_expected_champion_identity(pointer)
        except ValueError as exc:
            assert "duplicate JSON key" in str(exc)
        else:
            raise AssertionError("ambiguous champion pointer was accepted")

        pointer.write_bytes(b"x" * (_client.MAX_POINTER_BYTES + 1))
        try:
            _client.load_expected_champion_identity(pointer)
        except ValueError as exc:
            assert "no larger than" in str(exc)
        else:
            raise AssertionError("oversized champion pointer was accepted")


def test_result_marker_persists_exact_served_identity() -> None:
    response = _identity_response(transcript="دەقی دروست")
    text, model_id, deployment_sha = _client.validate_transcription_response(response)
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        _client.emit(text, model_id, deployment_sha)
    line = output.getvalue().strip()
    assert line.startswith("__RESULT__=")
    parsed = json.loads(line.removeprefix("__RESULT__="))
    assert parsed == {
        "raw_transcript": "دەقی دروست",
        "confidence": None,
        "model_version_id": "omniasr-7b-test@0123456789ab",
        "deployment_sha256": "1" * 64,
    }


def test_missing_or_partial_identity_is_rejected() -> None:
    invalid = [
        _identity_response(modelVersionId=""),
        _identity_response(deploymentSha256="not-a-hash"),
        _identity_response(componentSha256={"base": "3" * 64}),
        _identity_response(protocol="some-other-service"),
        _identity_response(language="ckb"),
    ]
    for response in invalid:
        try:
            _client.validate_identity_response(response)
        except ValueError:
            pass
        else:
            raise AssertionError(f"invalid server identity was accepted: {response!r}")


def test_busy_replica_is_retried_until_an_identity_bound_reply_arrives() -> None:
    original = _client._request_server_once
    calls = []
    try:
        def request_once(request, _timeout):
            calls.append(request)
            if len(calls) < 3:
                raise _client.ServerBusy("replica busy")
            return _identity_response(transcript="done")

        _client._request_server_once = request_once
        response = _client.request_server({"op": "transcribe", "audio_path": "/tmp/a.wav"}, 1)
        assert response["transcript"] == "done"
        assert len(calls) == 3
    finally:
        _client._request_server_once = original


if __name__ == "__main__":
    test_health_mode_never_resolves_the_database()
    test_health_mode_is_bound_to_the_exact_current_pointer()
    test_well_formed_stale_loopback_listener_cannot_claim_ready()
    test_expected_pointer_is_bounded_and_duplicate_key_safe()
    test_result_marker_persists_exact_served_identity()
    test_missing_or_partial_identity_is_rejected()
    test_busy_replica_is_retried_until_an_identity_bound_reply_arrives()
    print("PASS: champion client deployment identity")
