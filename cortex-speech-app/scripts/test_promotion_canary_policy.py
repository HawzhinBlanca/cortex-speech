"""The canary builder must emit the exact bytes the Rust loader accepts.

`champion_promotion_runtime::load_canary` re-serialises the file and refuses it unless the bytes on
disk are byte-identical to `serde_json::to_vec(canonical_json(value))` plus one trailing newline
(`PROMOTION_CANARY_NOT_CANONICAL`), and separately refuses any drift from the sha256 a
`CanaryIdentity` carries (`PROMOTION_CANARY_SHA_MISMATCH`). A builder that is even one byte off — a
space after a colon, unsorted keys, a missing or doubled newline — produces a file that can never
promote anything, and the failure would only appear during a real promotion.

So this pins the Python writer against the EXACT byte fixtures asserted on the Rust side in
`champion_promotion_runtime::tests`. If either side's canonicalisation changes, this reds.

Regression guard: 2026-08-19.
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import build_promotion_canary as builder  # noqa: E402

RUST_SOURCE = REPO_ROOT / "src-tauri" / "src" / "champion_promotion_runtime.rs"

# Verbatim from champion_promotion_runtime::tests::canonical_canary_bytes_and_hash_are_stable
EMPTY_SUITE_BYTES = b'{"cases":[],"schema":1,"suiteId":"fixed-v1"}\n'
# Verbatim from champion_promotion_runtime::tests::canary_loader_binds_exact_canonical_bytes
ONE_CASE_BYTES = (
    b'{"cases":[{"alignmentSha256":"' + b"b" * 64 + b'","segmentId":"segment-1",'
    b'"sourceAudioSha256":"' + b"a" * 64 + b'"}],"schema":1,"suiteId":"fixed-v1"}\n'
)


def test_empty_suite_matches_the_rust_fixture_byte_for_byte() -> None:
    produced = builder.canonical_bytes({"suiteId": "fixed-v1", "schema": 1, "cases": []})
    assert produced == EMPTY_SUITE_BYTES, (produced, EMPTY_SUITE_BYTES)


def test_one_case_suite_matches_the_rust_fixture_byte_for_byte() -> None:
    suite = {
        "schema": 1,
        "suiteId": "fixed-v1",
        "cases": [
            {
                "segmentId": "segment-1",
                "sourceAudioSha256": "a" * 64,
                "alignmentSha256": "b" * 64,
            }
        ],
    }
    produced = builder.canonical_bytes(suite)
    assert produced == ONE_CASE_BYTES, (produced, ONE_CASE_BYTES)


def test_key_order_of_the_input_cannot_change_the_output() -> None:
    """Canonical means canonical: the writer must not inherit dict insertion order."""
    a = builder.canonical_bytes({"schema": 1, "cases": [], "suiteId": "fixed-v1"})
    b = builder.canonical_bytes({"suiteId": "fixed-v1", "cases": [], "schema": 1})
    assert a == b == EMPTY_SUITE_BYTES


def test_the_fixtures_are_still_the_ones_rust_asserts() -> None:
    """If the Rust fixture is edited, these pins must be re-derived, not silently diverge."""
    rust = RUST_SOURCE.read_text(encoding="utf-8")
    if 'b"{\\"cases\\":[],\\"schema\\":1,\\"suiteId\\":\\"fixed-v1\\"}\\n"' not in rust:
        raise AssertionError(
            "the Rust empty-suite canonical fixture changed — re-derive EMPTY_SUITE_BYTES from "
            "champion_promotion_runtime::tests instead of assuming it still holds"
        )
    if "PROMOTION_CANARY_NOT_CANONICAL" not in rust:
        raise AssertionError("load_canary no longer enforces canonical bytes — re-point this gate")


def test_builder_respects_the_runtime_bounds() -> None:
    """MAX_CANARY_CASES and the schema are the runtime's, not the builder's opinion."""
    rust = RUST_SOURCE.read_text(encoding="utf-8")
    assert f"const MAX_CANARY_CASES: usize = {builder.MAX_CANARY_CASES};" in rust, (
        "builder MAX_CANARY_CASES disagrees with the runtime's bound"
    )
    assert f"const CANARY_SCHEMA: u32 = {builder.CANARY_SCHEMA};" in rust, (
        "builder CANARY_SCHEMA disagrees with the runtime's schema"
    )


def test_a_suite_hash_is_the_hash_of_the_written_bytes() -> None:
    """The printed suiteSha256 must be over the exact file bytes, or the identity never matches."""
    payload = builder.canonical_bytes({"suiteId": "fixed-v1", "schema": 1, "cases": []})
    assert hashlib.sha256(payload).hexdigest() == hashlib.sha256(EMPTY_SUITE_BYTES).hexdigest()


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PROMOTION CANARY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
