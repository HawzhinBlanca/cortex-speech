#!/usr/bin/env python3
"""Identity-contract tests for cortex_7b_client.py (no DB, WSL, or model required)."""

import contextlib
import importlib.util
import io
import json
import sys
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
    test_result_marker_persists_exact_served_identity()
    test_missing_or_partial_identity_is_rejected()
    test_busy_replica_is_retried_until_an_identity_bound_reply_arrives()
    print("PASS: champion client deployment identity")
