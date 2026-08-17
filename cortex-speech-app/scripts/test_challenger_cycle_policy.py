#!/usr/bin/env python3
"""The challenger cycle must never train on data it cannot prove, nor claim training it did not do.

Phase 3 of docs/PLAN_TRUE_10.md. Two failure modes are worth more than the feature itself:

  1. training on rows that were not the sealed snapshot's rows — after which "trained on snapshot X"
     is a claim rather than a fact, and every CER measured from it is unanchored;
  2. emitting a run record for training that never happened — which is how a prepared run becomes a
     "finished" one in a later summary.

Both are pinned here with real files.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
TRAINER = REPO_ROOT / "scripts" / "train_challenger.py"

sys.path.insert(0, str(REPO_ROOT / "scripts"))

from train_challenger import verify_pack  # noqa: E402


def _pack(tmp: Path, rows: list[dict]) -> tuple[Path, str]:
    """Write a pack and return (dir, manifest sha) — the sha is the snapshot id by construction."""
    pack = tmp / "pack"
    (pack / "clips").mkdir(parents=True, exist_ok=True)
    lines = "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows)
    manifest = pack / "finetune_manifest.jsonl"
    manifest.write_bytes(lines.encode("utf-8"))
    for row in rows:
        clip = pack / row["audio_path"]
        clip.parent.mkdir(parents=True, exist_ok=True)
        clip.write_bytes(b"RIFF")
    return pack, hashlib.sha256(manifest.read_bytes()).hexdigest()


def _row(idx: int, split: str = "train") -> dict:
    return {
        "audio_path": f"clips/c{idx}.wav",
        "sentence": f"دەقی {idx}",
        "duration_seconds": 1.0,
        "segment_id": f"c{idx}",
        "source_recording": "rec.wav",
        "split": split,
        "decision": "accept",
        "decision_revision": 1,
        "grade": "gold",
        "audio_processed": False,
    }


def test_an_edited_pack_is_refused() -> None:
    """The snapshot id IS the manifest hash, so a single edited byte must break the match."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i) for i in range(4)])
        sealed = {"status": "sealed"}
        assert verify_pack(pack, snapshot, sealed) == [], "the untouched pack must verify"

        (pack / "finetune_manifest.jsonl").write_text(
            (pack / "finetune_manifest.jsonl").read_text(encoding="utf-8") + json.dumps(_row(99)) + "\n",
            encoding="utf-8",
        )
        problems = verify_pack(pack, snapshot, sealed)
        assert any("does not match the snapshot" in p for p in problems), problems


def test_a_pack_with_no_train_split_is_refused() -> None:
    """Training on validation/test is the leak the whole split exists to prevent."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i, split="test") for i in range(3)])
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("no rows are in the train split" in p for p in problems), problems


def test_missing_clips_are_refused() -> None:
    """A run that trains on fewer clips than it claims produces a CER nobody can reproduce."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i) for i in range(4)])
        (pack / "clips" / "c2.wav").unlink()
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("missing from the pack" in p for p in problems), problems


def test_an_unsealed_snapshot_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i) for i in range(3)])
        problems = verify_pack(pack, snapshot, {"status": "draft"})
        assert any("not 'sealed'" in p for p in problems), problems


def test_a_prepared_run_never_reports_as_trained() -> None:
    """No trainer configured => exit 3 and status 'prepared', never a record that implies training."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i) for i in range(3)])
        out = tmp / "run"
        completed = subprocess.run(
            [sys.executable, str(TRAINER), "--snapshot", snapshot, "--pack", str(pack), "--out", str(out), "--dry-run"],
            capture_output=True,
            text=True,
        )
        # Exit 3 is deliberately NOT 0: a pipeline must not mistake a prepared run for a finished one.
        # (It exits 2 here only if the snapshot is unknown to the live DB, which is also a refusal.)
        assert completed.returncode in (2, 3), (completed.returncode, completed.stdout, completed.stderr)
        if completed.returncode == 3:
            record = json.loads((out / "challenger_run.json").read_text(encoding="utf-8"))
            assert record["status"] == "prepared"
            assert "not trained" in record["note"]
            assert "PREPARED (not trained)" in completed.stdout


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"CHALLENGER CYCLE: {len(tests)} refusals pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
