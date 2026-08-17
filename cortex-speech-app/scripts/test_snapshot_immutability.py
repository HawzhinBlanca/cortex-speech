#!/usr/bin/env python3
"""Pins for gate C's pure core — every way a sealed snapshot could stop being what it was."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_snapshot_immutability import check  # noqa: E402

GOOD_ID = hashlib.sha256(b"rows").hexdigest()


def _config(snapshot_id: str) -> str:
    return json.dumps({"snapshotId": snapshot_id, "manifestSha256": snapshot_id, "emitted": 3})


def test_a_well_formed_snapshot_passes() -> None:
    assert check([(GOOD_ID, "sealed", _config(GOOD_ID))], None) == []


def test_an_id_that_is_not_a_content_hash_fails() -> None:
    """A label can be reused for different rows; a hash cannot."""
    problems = check([("nightly-run-3", "sealed", _config("nightly-run-3"))], None)
    assert any("not a sha256" in p for p in problems), problems


def test_a_config_naming_a_different_id_fails() -> None:
    """The row says one thing and its own record says another — one of them is wrong."""
    other = hashlib.sha256(b"other").hexdigest()
    problems = check([(GOOD_ID, "sealed", _config(other))], None)
    assert any("DIFFERENT id" in p for p in problems), problems


def test_an_unsealed_status_fails() -> None:
    problems = check([(GOOD_ID, "draft", _config(GOOD_ID))], None)
    assert any("not 'sealed'" in p for p in problems), problems


def test_an_unparseable_config_fails() -> None:
    problems = check([(GOOD_ID, "sealed", "{not json")], None)
    assert any("unparseable" in p for p in problems), problems


def test_a_pack_edited_after_sealing_fails() -> None:
    """The check that turns 'trained on snapshot X' from a claim into a fact."""
    with tempfile.TemporaryDirectory() as raw:
        runs = Path(raw)
        pack = runs / f"challenger_{GOOD_ID[:12]}"
        pack.mkdir(parents=True)
        manifest = pack / "finetune_manifest.jsonl"

        manifest.write_bytes(b"rows")  # hashes to GOOD_ID by construction
        assert check([(GOOD_ID, "sealed", _config(GOOD_ID))], runs) == [], "the untouched pack must verify"

        manifest.write_bytes(b"rows and one more")
        problems = check([(GOOD_ID, "sealed", _config(GOOD_ID))], runs)
        assert any("edited after sealing" in p for p in problems), problems


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"SNAPSHOT IMMUTABILITY CORE: {len(tests)} invariants pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
