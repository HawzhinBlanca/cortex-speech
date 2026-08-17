#!/usr/bin/env python3
"""Train a challenger LoRA from ONE sealed dataset snapshot — reproducibly, or not at all.

    python scripts/train_challenger.py --snapshot <id> --pack <pack_dir> [--out runs/<name>]

Phase 3 of docs/PLAN_TRUE_10.md. Training itself stays EXTERNAL by design (docs/RETRAIN_RUNBOOK.md:
the app's job is to produce leak-clean data and to gate the result, not to own the training loop).
What was missing is everything AROUND the training run:

  * the pack it trains on must be the one the snapshot sealed, byte for byte;
  * the run must record what it trained on and from what base, so the resulting adapter can be
    traced back to its exact rows;
  * a run that cannot be reproduced must not happen at all.

So this validates, records, and hands off. It NEVER fabricates a training run: with no trainer
configured it prepares the run and stops, saying so, rather than emitting a run record for training
that did not occur.

CANON: the challenger is a LoRA on the SAME champion family. This script cannot and must not select
a different model family — the owner's model lock is not a default, it is the rule
(cortex-speech-app/CLAUDE.md, "Model lock").
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sqlite3
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def sealed_snapshot(snapshot_id: str) -> dict | None:
    """The sealed record for this snapshot, or None if the id was never sealed."""
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        return None
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        row = con.execute(
            "SELECT name, status, config_json, created_at FROM dataset_runs WHERE id = ?1",
            (snapshot_id,),
        ).fetchone()
    finally:
        con.close()
    if not row:
        return None
    name, status, config_json, created_at = row
    try:
        config = json.loads(config_json)
    except ValueError:
        config = {"unparseable": config_json}
    return {"name": name, "status": status, "config": config, "created_at": created_at}


def sha256_of(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def verify_pack(pack_dir: Path, snapshot_id: str, sealed: dict) -> list[str]:
    """Every reason this pack may NOT be trained on. Empty list = safe to train."""
    problems: list[str] = []
    manifest = pack_dir / "finetune_manifest.jsonl"
    if not manifest.is_file():
        problems.append(f"no finetune_manifest.jsonl in {pack_dir}")
        return problems

    # THE identity check: the snapshot id IS the manifest hash, so this proves the rows on disk are
    # the rows that were sealed. Without it, "trained on snapshot X" is a claim, not a fact.
    actual = sha256_of(manifest)
    if actual != snapshot_id:
        problems.append(
            f"pack does not match the snapshot: manifest hashes to {actual[:16]}… but the snapshot id "
            f"is {snapshot_id[:16]}…. Either the pack was edited or it is a different export"
        )

    rows = [json.loads(line) for line in manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not rows:
        problems.append("the pack is empty — there is nothing to train on")
        return problems

    # Leakage: nothing marked for evaluation may be trained on. The pack's own split says which.
    train_rows = [r for r in rows if r.get("split") == "train"]
    if not train_rows:
        problems.append("no rows are in the train split — training on validation/test is the leak this gates")
    missing_split = [r for r in rows if "split" not in r]
    if missing_split:
        problems.append(
            f"{len(missing_split)} row(s) carry no split — a row with no split cannot be shown to be "
            "safe to train on"
        )

    # Every clip must exist, or the run trains on less than it claims.
    absent = [r for r in train_rows if not (pack_dir / r["audio_path"]).is_file()]
    if absent:
        problems.append(f"{len(absent)} train clip(s) named in the manifest are missing from the pack")

    if sealed["status"] != "sealed":
        problems.append(f"snapshot status is {sealed['status']!r}, not 'sealed'")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description="Train a challenger LoRA from a sealed snapshot")
    parser.add_argument("--snapshot", required=True, help="dataset snapshot id (the manifest hash)")
    parser.add_argument("--pack", required=True, type=Path, help="the exported pack directory")
    parser.add_argument("--out", type=Path, help="where to write the run record + adapter")
    parser.add_argument(
        "--trainer",
        default=os.environ.get("CORTEX_CHALLENGER_TRAINER", ""),
        help="command that performs the fine-tune. Receives --manifest/--out/--base. Training is "
        "external by design; with no trainer configured this prepares the run and stops.",
    )
    parser.add_argument(
        "--base",
        default=os.environ.get("CORTEX_CHALLENGER_BASE", ""),
        help="base checkpoint the LoRA adapts. MUST be the champion family (owner's model lock).",
    )
    parser.add_argument("--dry-run", action="store_true", help="validate and record, never invoke the trainer")
    args = parser.parse_args()

    sealed = sealed_snapshot(args.snapshot)
    if sealed is None:
        print(f"CHALLENGER: REFUSED — snapshot {args.snapshot[:16]}… was never sealed", flush=True)
        print("  A training run must cite a sealed snapshot, or nothing can say what it learned from.", flush=True)
        return 2

    problems = verify_pack(args.pack, args.snapshot, sealed)
    if problems:
        print("CHALLENGER: REFUSED — the pack cannot be trained on as-is", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 2

    manifest = args.pack / "finetune_manifest.jsonl"
    rows = [json.loads(line) for line in manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    by_split: dict[str, int] = {}
    for row in rows:
        by_split[row.get("split", "?")] = by_split.get(row.get("split", "?"), 0) + 1

    out_dir = args.out or (REPO_ROOT / "runs" / f"challenger_{args.snapshot[:12]}")
    out_dir.mkdir(parents=True, exist_ok=True)

    record = {
        "schema": 1,
        "snapshot_id": args.snapshot,
        "snapshot_sealed_at": sealed["created_at"],
        "snapshot_selection": sealed["config"],
        "pack_dir": str(args.pack.resolve()),
        "manifest_sha256": args.snapshot,
        "rows_total": len(rows),
        "rows_by_split": by_split,
        "base_checkpoint": args.base or None,
        "trainer_command": args.trainer or None,
        "family_lock": "LoRA on the champion family only — a different family invalidates every "
        "measured CER on the frozen eval set (owner's model lock)",
    }

    print(f"CHALLENGER: snapshot {args.snapshot[:16]}… verified against the pack", flush=True)
    for split, count in sorted(by_split.items()):
        print(f"  {split:12s} {count:6d} rows", flush=True)

    if not args.base:
        print("  no --base checkpoint given (CORTEX_CHALLENGER_BASE): recording the run only", flush=True)
    if args.dry_run or not args.trainer:
        record["status"] = "prepared"
        record["note"] = (
            "no trainer configured — the run was PREPARED and validated, not trained. Training is "
            "external by design; set --trainer or CORTEX_CHALLENGER_TRAINER to actually run it."
        )
        (out_dir / "challenger_run.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"CHALLENGER: PREPARED (not trained) — record at {out_dir / 'challenger_run.json'}", flush=True)
        # Exit 3 = prepared-but-not-trained. Distinct from success, so a pipeline can never mistake a
        # dry run for a finished one.
        return 3

    command = [
        *args.trainer.split(),
        "--manifest", str(manifest),
        "--out", str(out_dir),
        *(["--base", args.base] if args.base else []),
    ]
    print(f"  invoking: {' '.join(command)}", flush=True)
    completed = subprocess.run(command, cwd=REPO_ROOT)
    record["status"] = "trained" if completed.returncode == 0 else "failed"
    record["trainer_exit_code"] = completed.returncode
    (out_dir / "challenger_run.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")

    if completed.returncode != 0:
        print(f"CHALLENGER: TRAINING FAILED (exit {completed.returncode}) — recorded, not hidden", flush=True)
        return 1
    print(f"CHALLENGER: trained — record at {out_dir / 'challenger_run.json'}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
