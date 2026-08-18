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
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
TRAINER = REPO_ROOT / "scripts" / "train_challenger.py"

sys.path.insert(0, str(REPO_ROOT / "scripts"))

from train_challenger import (  # noqa: E402
    build_deployment_manifest,
    sha256_of,
    verify_base_checkpoint,
    verify_deployment_manifest,
    verify_pack,
    verify_trainer_outputs,
)
from check_challenger_loop import audit_run  # noqa: E402


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
    snapshot = hashlib.sha256(manifest.read_bytes()).hexdigest()
    provenance = {
        "schema": 2,
        "snapshotId": snapshot,
        "manifestSha256": snapshot,
        "emitted": len(rows),
    }
    (pack / "pack_provenance.json").write_text(
        json.dumps(provenance, indent=2), encoding="utf-8", newline="\n"
    )
    files = [manifest, pack / "pack_provenance.json", *[pack / row["audio_path"] for row in rows]]
    sums = "".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(pack).as_posix()}\n"
        for path in sorted(files, key=lambda value: value.relative_to(pack).as_posix())
    )
    (pack / "SHA256SUMS").write_text(sums, encoding="utf-8", newline="\n")
    return pack, snapshot


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
        pack, snapshot = _pack(tmp, [_row(0), _row(1), _row(2), _row(3, "validation")])
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
        assert any("missing or escapes" in p for p in problems), problems


def test_missing_validation_clip_is_also_refused() -> None:
    """All split files are part of the immutable snapshot, even though only train reaches the trainer."""
    with tempfile.TemporaryDirectory() as raw:
        pack, snapshot = _pack(Path(raw), [_row(1), _row(2, "validation")])
        (pack / "clips" / "c2.wav").unlink()
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("clips/c2.wav" in problem for problem in problems), problems


def test_modified_wav_is_refused_by_complete_root_hashing() -> None:
    with tempfile.TemporaryDirectory() as raw:
        pack, snapshot = _pack(Path(raw), [_row(1), _row(2)])
        (pack / "clips" / "c2.wav").write_bytes(b"RIFF-tampered")
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("clips/c2.wav no longer matches SHA256SUMS" in problem for problem in problems), problems


def test_modified_provenance_is_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        pack, snapshot = _pack(Path(raw), [_row(1), _row(2)])
        provenance = pack / "pack_provenance.json"
        value = json.loads(provenance.read_text(encoding="utf-8"))
        value["emitted"] = 999
        provenance.write_text(json.dumps(value), encoding="utf-8")
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("emitted=999" in problem for problem in problems), problems
        assert any("pack_provenance.json no longer matches SHA256SUMS" in problem for problem in problems), problems


def test_crlf_manifest_drift_is_explicitly_refused() -> None:
    with tempfile.TemporaryDirectory() as raw:
        pack, snapshot = _pack(Path(raw), [_row(1), _row(2)])
        manifest = pack / "finetune_manifest.jsonl"
        manifest.write_bytes(manifest.read_bytes().replace(b"\n", b"\r\n"))
        problems = verify_pack(pack, snapshot, {"status": "sealed"})
        assert any("CRLF/CR byte drift" in problem for problem in problems), problems
        assert any("does not match the snapshot" in problem for problem in problems), problems


def test_exact_base_hash_is_mandatory() -> None:
    with tempfile.TemporaryDirectory() as raw:
        base = Path(raw) / "base.safetensors"
        base.write_bytes(b"exact-base")
        problems, _ = verify_base_checkpoint(str(base), "registry-champion-v1", "")
        assert any("base-sha256" in problem for problem in problems), problems
        expected = hashlib.sha256(base.read_bytes()).hexdigest()
        assert verify_base_checkpoint(str(base), "registry-champion-v1", expected)[0] == []


def test_exit_zero_without_evidence_cannot_become_trained() -> None:
    """Postflight needs consumption and real artifacts; process success is deliberately irrelevant."""
    with tempfile.TemporaryDirectory() as raw:
        out = Path(raw)
        contract = {
            "snapshot": {"id": "a" * 64, "root_sha256": "b" * 64},
            "train_manifest": {
                "sha256": "c" * 64,
                "rows": 3,
                "audio_files_count": 3,
                "input_root_sha256": "2" * 64,
            },
            "base": {"id": "base", "sha256": "d" * 64},
        }
        problems, _ = verify_trainer_outputs(out, contract, "e" * 64)
        assert any("trainer_evidence.json" in problem for problem in problems), problems
        assert any("artifact_manifest.json" in problem for problem in problems), problems


def test_complete_artifacts_emit_registry_server_manifest() -> None:
    with tempfile.TemporaryDirectory() as raw:
        out = Path(raw)
        base_path = out / "base.safetensors"
        base_path.write_bytes(b"base")
        base_sha = sha256_of(base_path)
        contract = {
            "snapshot": {"id": "a" * 64, "root_sha256": "b" * 64},
            "train_manifest": {
                "sha256": "c" * 64,
                "rows": 3,
                "audio_files_count": 3,
                "input_root_sha256": "2" * 64,
            },
            "base": {
                "id": "base",
                "path": str(base_path),
                "sha256": base_sha,
                "size_bytes": 4,
            },
            "model_card": "soranivoice_omniASR_LLM_7B_v2_local",
        }
        contract_sha = "e" * 64
        identity = {
            "snapshot_id": "a" * 64,
            "snapshot_root_sha256": "b" * 64,
            "training_contract_sha256": contract_sha,
            "train_manifest_sha256": "c" * 64,
            "train_rows_consumed": 3,
            "train_audio_files_consumed": 3,
            "train_input_root_sha256": "2" * 64,
            "base_id": "base",
            "base_sha256": base_sha,
        }
        evidence = {
            **identity,
            "schema": 1,
            "status": "completed",
            "trainer_identity": "trainer.py@sha256:123",
            "recipe_sha256": "f" * 64,
            "environment_sha256": "1" * 64,
            "seed": 17,
        }
        (out / "trainer_evidence.json").write_text(json.dumps(evidence), encoding="utf-8")
        declared = []
        for role, name, payload in (
            ("adapter", "adapter_model.safetensors", b"adapter"),
            ("tokenizer", "tokenizer.json", b"tokenizer"),
            ("adapter_config", "adapter_config.json", b"{}"),
        ):
            path = out / name
            path.write_bytes(payload)
            declared.append(
                {"role": role, "path": name, "sha256": sha256_of(path), "size_bytes": len(payload)}
            )
        artifact_manifest = {
            **{
                key: value
                for key, value in identity.items()
                if key not in {"train_rows_consumed", "train_audio_files_consumed"}
            },
            "schema": 1,
            "artifacts": declared,
        }
        (out / "artifact_manifest.json").write_text(json.dumps(artifact_manifest), encoding="utf-8")
        problems, outputs = verify_trainer_outputs(out, contract, contract_sha)
        assert problems == [], problems
        deployment = build_deployment_manifest(contract, contract_sha, outputs, "trainer.py")
        assert deployment["family"] == "omniasr-7b"
        assert deployment["language"] == "ckb_Arab"
        assert deployment["runtime"]["vocab_size"] == 10288
        (out / "deployment_manifest.json").write_bytes(
            json.dumps(deployment, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        )
        assert verify_deployment_manifest(out / "deployment_manifest.json", out) == []
        (out / "adapter_model.safetensors").write_bytes(b"tampered")
        assert any("hash does not match" in problem for problem in verify_trainer_outputs(out, contract, contract_sha)[0])
        assert any(
            "adapter file does not match declared sha256" in problem
            for problem in verify_deployment_manifest(out / "deployment_manifest.json", out)
        )
        wrong = out / "wrong_adapter.safetensors"
        wrong.write_bytes(b"wrong-but-hashed")
        artifact_manifest["artifacts"][0] = {
            "role": "adapter",
            "path": wrong.name,
            "sha256": sha256_of(wrong),
            "size_bytes": wrong.stat().st_size,
        }
        (out / "artifact_manifest.json").write_text(json.dumps(artifact_manifest), encoding="utf-8")
        assert any(
            "adapter_model.safetensors" in problem
            for problem in verify_trainer_outputs(out, contract, contract_sha)[0]
        )


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


def test_real_wrapper_rejects_a_fake_successful_trainer() -> None:
    """Integration pin: an actual exit-0 process with no evidence/artifacts leaves status failed."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(i) for i in range(3)])
        appdata = tmp / "appdata"
        db_dir = appdata / "cortex-speech"
        db_dir.mkdir(parents=True)
        con = sqlite3.connect(db_dir / "cortex-speech.db")
        con.execute(
            "CREATE TABLE dataset_runs "
            "(id TEXT PRIMARY KEY, name TEXT, status TEXT, config_json TEXT, created_at TEXT)"
        )
        config = {
            "schema": 2,
            "snapshotId": snapshot,
            "manifestSha256": snapshot,
            "emitted": 3,
        }
        # The pack helper's provenance has the same sealed selection fields.
        (pack / "pack_provenance.json").write_text(json.dumps(config), encoding="utf-8")
        files = [
            pack / "finetune_manifest.jsonl",
            pack / "pack_provenance.json",
            *sorted((pack / "clips").glob("*.wav")),
        ]
        (pack / "SHA256SUMS").write_text(
            "".join(
                f"{sha256_of(path)}  {path.relative_to(pack).as_posix()}\n"
                for path in sorted(files, key=lambda value: value.relative_to(pack).as_posix())
            ),
            encoding="utf-8",
            newline="\n",
        )
        con.execute(
            "INSERT INTO dataset_runs VALUES (?1,'finetune-pack','sealed',?2,'2026-08-18T00:00:00Z')",
            (snapshot, json.dumps(config)),
        )
        con.commit()
        con.close()
        base = tmp / "base.safetensors"
        base.write_bytes(b"exact pinned base")
        fake = tmp / "fake_success.py"
        fake.write_text("raise SystemExit(0)\n", encoding="utf-8")
        out = tmp / "run"
        completed = subprocess.run(
            [
                sys.executable,
                str(TRAINER),
                "--snapshot",
                snapshot,
                "--pack",
                str(pack),
                "--out",
                str(out),
                "--trainer",
                json.dumps([sys.executable, str(fake)]),
                "--base",
                str(base),
                "--base-id",
                "registry-champion-v1",
                "--base-sha256",
                sha256_of(base),
                "--model-card",
                "soranivoice_omniASR_LLM_7B_v2_local",
            ],
            env={**os.environ, "APPDATA": str(appdata)},
            capture_output=True,
            text=True,
        )
        assert completed.returncode == 1, (completed.returncode, completed.stdout, completed.stderr)
        record = json.loads((out / "challenger_run.json").read_text(encoding="utf-8"))
        assert record["status"] == "failed"
        assert record["trainer_exit_code"] == 0
        assert "exit 0 alone is not training proof" in completed.stdout


def test_real_wrapper_accepts_only_complete_hashed_training_evidence() -> None:
    """A complete fake contract exercises the same file/evidence boundary as the real GPU trainer."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pack, snapshot = _pack(tmp, [_row(0), _row(1), _row(2), _row(3, "validation")])
        appdata = tmp / "appdata"
        db_dir = appdata / "cortex-speech"
        db_dir.mkdir(parents=True)
        config = {
            "schema": 2,
            "snapshotId": snapshot,
            "manifestSha256": snapshot,
            "emitted": 4,
        }
        (pack / "pack_provenance.json").write_text(json.dumps(config), encoding="utf-8")
        files = [
            pack / "finetune_manifest.jsonl",
            pack / "pack_provenance.json",
            *sorted((pack / "clips").glob("*.wav")),
        ]
        (pack / "SHA256SUMS").write_text(
            "".join(
                f"{sha256_of(path)}  {path.relative_to(pack).as_posix()}\n"
                for path in sorted(files, key=lambda value: value.relative_to(pack).as_posix())
            ),
            encoding="utf-8",
            newline="\n",
        )
        con = sqlite3.connect(db_dir / "cortex-speech.db")
        con.execute(
            "CREATE TABLE dataset_runs "
            "(id TEXT PRIMARY KEY, name TEXT, status TEXT, config_json TEXT, created_at TEXT)"
        )
        con.execute(
            "INSERT INTO dataset_runs VALUES (?1,'finetune-pack','sealed',?2,'2026-08-18T00:00:00Z')",
            (snapshot, json.dumps(config)),
        )
        con.commit()
        con.close()
        base = tmp / "base.safetensors"
        base.write_bytes(b"exact pinned base")
        fake = tmp / "complete_trainer.py"
        fake.write_text(
            """
import argparse
import hashlib
import json
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("--manifest")
p.add_argument("--out")
p.add_argument("--base")
p.add_argument("--contract")
a = p.parse_args()
out = Path(a.out)
contract_path = Path(a.contract)
contract = json.loads(contract_path.read_text(encoding="utf-8"))
h = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
artifacts = []
for role, name, payload in (
    ("adapter", "adapter_model.safetensors", b"trained-adapter"),
    ("tokenizer", "tokenizer.json", b"trained-tokenizer"),
    ("adapter_config", "adapter_config.json", b'{"r":16}'),
):
    path = out / name
    path.write_bytes(payload)
    artifacts.append({"role": role, "path": name, "sha256": h(path), "size_bytes": len(payload)})
common = {
    "snapshot_id": contract["snapshot"]["id"],
    "snapshot_root_sha256": contract["snapshot"]["root_sha256"],
    "training_contract_sha256": h(contract_path),
    "train_manifest_sha256": h(a.manifest),
    "train_input_root_sha256": contract["train_manifest"]["input_root_sha256"],
    "base_id": contract["base"]["id"],
    "base_sha256": h(a.base),
}
evidence = {
    **common,
    "schema": 1,
    "status": "completed",
    "train_rows_consumed": contract["train_manifest"]["rows"],
    "train_audio_files_consumed": contract["train_manifest"]["audio_files_count"],
    "trainer_identity": "complete_trainer.py:test-fixture",
    "recipe_sha256": "f" * 64,
    "environment_sha256": "e" * 64,
    "seed": 42,
}
(out / "trainer_evidence.json").write_text(json.dumps(evidence), encoding="utf-8")
(out / "artifact_manifest.json").write_text(
    json.dumps({**common, "schema": 1, "artifacts": artifacts}), encoding="utf-8"
)
""".lstrip(),
            encoding="utf-8",
        )
        out = tmp / "run"
        command = [
            sys.executable,
            str(TRAINER),
            "--snapshot",
            snapshot,
            "--pack",
            str(pack),
            "--out",
            str(out),
            "--trainer",
            json.dumps([sys.executable, str(fake)]),
            "--base",
            str(base),
            "--base-id",
            "registry-champion-v1",
            "--base-sha256",
            sha256_of(base),
            "--model-card",
            "soranivoice_omniASR_LLM_7B_v2_local",
        ]
        completed = subprocess.run(
            command,
            env={**os.environ, "APPDATA": str(appdata)},
            capture_output=True,
            text=True,
        )
        assert completed.returncode == 0, (completed.returncode, completed.stdout, completed.stderr)
        record = json.loads((out / "challenger_run.json").read_text(encoding="utf-8"))
        assert record["status"] == "trained"
        assert (out / "deployment_manifest.json").is_file()
        assert not (out / "training_input" / "clips" / "c3.wav").exists()
        assert audit_run(record, out) == []


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"CHALLENGER CYCLE: {len(tests)} refusals pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
