#!/usr/bin/env python3
"""Regression tests for the champion server's bounded, validated socket protocol."""

import importlib.util
import json
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "cortex_7b_server_protocol", Path(__file__).parent / "cortex_7b_server.py"
)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)


class FakeConnection:
    def __init__(self, payload: bytes):
        self.payload = bytearray(payload)
        self.timeout = None

    def settimeout(self, timeout):
        self.timeout = timeout

    def recv(self, size: int) -> bytes:
        chunk = bytes(self.payload[:size])
        del self.payload[:size]
        return chunk


def test_protocol_bounds_input_before_parsing() -> None:
    conn = FakeConnection(b"x" * 65)
    try:
        _mod.read_bounded_json_request(conn, max_bytes=64)
    except ValueError as exc:
        assert "exceeds 64 bytes" in str(exc)
    else:
        raise AssertionError("oversized request was accepted")
    assert conn.timeout == _mod.REQUEST_READ_TIMEOUT_SECONDS


def test_protocol_accepts_only_valid_transcription_shapes() -> None:
    body = json.dumps({"audio_path": "/tmp/clip.wav", "start_ms": 0, "end_ms": 500}).encode() + b"\n"
    parsed = _mod.read_bounded_json_request(FakeConnection(body))
    assert _mod.validate_transcription_request(parsed) == ("/tmp/clip.wav", 0, 500)

    invalid = [
        {},
        {"audio_path": ""},
        {"audio_path": "/tmp/x.wav", "start_ms": 1},
        {"audio_path": "/tmp/x.wav", "start_ms": -1, "end_ms": 2},
        {"audio_path": "/tmp/x.wav", "start_ms": 5, "end_ms": 5},
        {"audio_path": "/tmp/x.wav", "start_ms": float("nan"), "end_ms": 5},
    ]
    for request in invalid:
        try:
            _mod.validate_transcription_request(request)
        except ValueError:
            pass
        else:
            raise AssertionError(f"invalid request was accepted: {request!r}")


if __name__ == "__main__":
    test_protocol_bounds_input_before_parsing()
    test_protocol_accepts_only_valid_transcription_shapes()
    print("PASS: champion server socket protocol")
