#!/usr/bin/env python3
"""Pin Couch Review's production transport and pairing boundary to TLS."""

from pathlib import Path

from _couch_policy_util import couch_surface


def test_production_couch_is_tls_and_pairing_based() -> None:
    source = couch_surface(Path(__file__).parents[1] / "src-tauri" / "src")
    start = source.index("fn start_on_port(")
    stop = source.index("/// Stop the couch server", start)
    production_start = source[start:stop]
    assert "tiny_http::Server::https" in production_start
    assert "tiny_http::Server::http(" not in production_start

    status_start = source.index("fn status_of(")
    status_end = source.index("fn lan_ip(", status_start)
    status = source[status_start:status_end]
    assert 'format!("https://' in status
    assert 'format!("http://' not in status

    claim_start = source.index("fn api_claim(")
    claim_end = source.index("fn json_reply", claim_start)
    claim = source[claim_start:claim_end]
    assert "session_token" in claim and "pairing_codes" in claim
    assert "Secure; HttpOnly" in claim


if __name__ == "__main__":
    test_production_couch_is_tls_and_pairing_based()
    print("PASS: Couch Review TLS/pairing policy")
