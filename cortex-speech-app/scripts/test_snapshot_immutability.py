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
from train_challenger import inspect_pack  # noqa: E402

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


def _pack(root: Path) -> tuple[Path, str, str]:
    pack = root / "pack_at_recorded_location"
    (pack / "clips").mkdir(parents=True)
    row = {
        "audio_path": "clips/one.wav",
        "sentence": "دەق",
        "duration_seconds": 1.0,
        "segment_id": "one",
        "source_recording": "source.wav",
        "split": "train",
        "decision": "accept",
        "decision_revision": 1,
        "grade": "gold",
        "audio_processed": False,
    }
    manifest = pack / "finetune_manifest.jsonl"
    manifest.write_text(json.dumps(row, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    (pack / "clips" / "one.wav").write_bytes(b"RIFF")
    snapshot = hashlib.sha256(manifest.read_bytes()).hexdigest()
    config = json.dumps({"snapshotId": snapshot, "manifestSha256": snapshot, "emitted": 1})
    provenance = {**json.loads(config), "schema": 2}
    (pack / "pack_provenance.json").write_text(json.dumps(provenance), encoding="utf-8")
    files = [manifest, pack / "pack_provenance.json", pack / "clips" / "one.wav"]
    sums = "".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(pack).as_posix()}\n"
        for path in sorted(files, key=lambda value: value.relative_to(pack).as_posix())
    )
    (pack / "SHA256SUMS").write_text(sums, encoding="utf-8", newline="\n")
    return pack, snapshot, config


def test_a_pack_edited_after_sealing_fails() -> None:
    """The checker follows recorded pack_dir, then validates the complete root."""
    with tempfile.TemporaryDirectory() as raw:
        runs = Path(raw)
        pack, snapshot, config = _pack(runs)
        _, evidence = inspect_pack(pack, snapshot, {"status": "sealed", "config": json.loads(config)})
        record_dir = runs / "challenger_record"
        record_dir.mkdir()
        (record_dir / "challenger_run.json").write_text(
            json.dumps(
                {
                    "snapshot_id": snapshot,
                    "pack_dir": str(pack.resolve()),
                    "snapshot_root_sha256": evidence["snapshot_root_sha256"],
                }
            ),
            encoding="utf-8",
        )
        assert check([(snapshot, "sealed", config)], runs) == [], "untouched recorded root must verify"

        manifest = pack / "finetune_manifest.jsonl"
        manifest.write_bytes(manifest.read_bytes().replace(b"\n", b"\r\n"))
        problems = check([(snapshot, "sealed", config)], runs)
        assert any("CRLF/CR byte drift" in problem for problem in problems), problems


def test_a_snapshot_without_recorded_pack_binding_fails_closed() -> None:
    with tempfile.TemporaryDirectory() as raw:
        problems = check([(GOOD_ID, "sealed", _config(GOOD_ID))], Path(raw))
        assert any("no challenger_run.json" in problem for problem in problems), problems


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"SNAPSHOT IMMUTABILITY CORE: {len(tests)} invariants pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
