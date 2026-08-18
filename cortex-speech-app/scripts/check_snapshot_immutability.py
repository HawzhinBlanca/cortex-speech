#!/usr/bin/env python3
"""Gate C: every recorded snapshot pack must still be the complete bytes it sealed."""

from __future__ import annotations

import json
import os
import sqlite3
from pathlib import Path
from typing import Any

from train_challenger import inspect_pack, is_sha256

REPO_ROOT = Path(__file__).resolve().parents[1]


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def discover_bindings(run_root: Path) -> tuple[dict[str, list[dict[str, Any]]], list[str]]:
    """Read pack roots from run records; directory-name guesses are not provenance."""
    bindings: dict[str, list[dict[str, Any]]] = {}
    problems: list[str] = []
    if not run_root.is_dir():
        return bindings, problems
    for record_path in sorted(run_root.rglob("challenger_run.json")):
        label = str(record_path)
        try:
            record = json.loads(record_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"{label} is unreadable: {error}")
            continue
        if not isinstance(record, dict):
            problems.append(f"{label} is not a JSON object")
            continue
        snapshot_id = record.get("snapshot_id")
        pack_dir = record.get("pack_dir")
        if not is_sha256(snapshot_id):
            problems.append(f"{label} has no canonical snapshot_id")
            continue
        if not isinstance(pack_dir, str) or not pack_dir:
            problems.append(f"{label} has no recorded pack_dir")
            continue
        path = Path(pack_dir)
        if not path.is_absolute():
            problems.append(f"{label} records relative pack_dir {pack_dir!r}")
            continue
        try:
            canonical = path.resolve(strict=True)
        except (FileNotFoundError, OSError, RuntimeError):
            problems.append(f"{label} recorded pack_dir no longer exists: {pack_dir}")
            continue
        if str(path) != str(canonical):
            problems.append(f"{label} pack_dir is not its canonical resolved path: {pack_dir}")
        bindings.setdefault(snapshot_id, []).append(
            {
                "pack_dir": str(canonical),
                "snapshot_root_sha256": record.get("snapshot_root_sha256"),
                "record_path": label,
            }
        )
    return bindings, problems


def check(
    rows: list[tuple[str, str, str]],
    binding_source: Path | dict[str, list[dict[str, Any]]] | None,
    *,
    require_bindings: bool = False,
) -> list[str]:
    """Return every invariant violation. None keeps DB-only unit checks possible."""
    problems: list[str] = []
    if isinstance(binding_source, Path):
        bindings, discovery_problems = discover_bindings(binding_source)
        problems.extend(discovery_problems)
        require_bindings = True
    else:
        bindings = binding_source or {}
    seen: set[str] = set()
    known_ids = {row[0] for row in rows}
    for dangling in sorted(set(bindings) - known_ids):
        problems.append(f"run record cites snapshot {dangling[:16]}… which is absent from live sealed rows")

    for snapshot_id, status, config_json in rows:
        if not is_sha256(snapshot_id):
            problems.append(
                f"snapshot id {snapshot_id[:24]!r} is not a sha256 in canonical lowercase form — "
                "a label can be reused"
            )
        if snapshot_id in seen:
            problems.append(f"snapshot id {snapshot_id[:16]}… appears twice — sealing is not idempotent")
        seen.add(snapshot_id)
        try:
            config = json.loads(config_json)
            if not isinstance(config, dict):
                raise ValueError("top level is not an object")
        except (TypeError, ValueError) as error:
            problems.append(f"snapshot {snapshot_id[:16]}… has unparseable config: {error}")
            continue
        for key in ("snapshotId", "manifestSha256"):
            if config.get(key) != snapshot_id:
                problems.append(f"snapshot {snapshot_id[:16]}… config field {key} names a DIFFERENT id")
        if status != "sealed":
            problems.append(f"snapshot {snapshot_id[:16]}… has status {status!r}, not 'sealed'")

        snapshot_bindings = bindings.get(snapshot_id, [])
        if require_bindings and not snapshot_bindings:
            problems.append(f"snapshot {snapshot_id[:16]}… has no challenger_run.json binding to a recorded pack_dir")
        checked_roots: set[tuple[str, object]] = set()
        for binding in snapshot_bindings:
            root_key = (binding["pack_dir"], binding.get("snapshot_root_sha256"))
            if root_key in checked_roots:
                continue
            checked_roots.add(root_key)
            prefix = f"{Path(binding['record_path']).parent.name}: "
            expected_root = binding.get("snapshot_root_sha256")
            if not is_sha256(expected_root):
                problems.append(prefix + "run record has no canonical snapshot_root_sha256")
            pack_problems, evidence = inspect_pack(
                Path(binding["pack_dir"]),
                snapshot_id,
                {"name": "finetune-pack", "status": status, "config": config},
            )
            problems.extend(prefix + problem for problem in pack_problems)
            actual_root = evidence.get("snapshot_root_sha256")
            if is_sha256(expected_root) and actual_root != expected_root:
                problems.append(
                    prefix + f"recorded snapshot root changed ({str(actual_root)[:16]}… != {expected_root[:16]}…)"
                )
    return problems


def main() -> int:
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        print(f"SNAPSHOT IMMUTABILITY: SKIP-ENV (no library at {db})", flush=True)
        return 0
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        if not con.execute("SELECT name FROM sqlite_master WHERE name='dataset_runs'").fetchone():
            print("SNAPSHOT IMMUTABILITY: SKIP-ENV (dataset_runs not present)", flush=True)
            return 0
        rows = con.execute(
            "SELECT id, status, config_json FROM dataset_runs WHERE name='finetune-pack' ORDER BY id"
        ).fetchall()
    finally:
        con.close()
    if not rows:
        print("SNAPSHOT IMMUTABILITY: SKIP-ENV (no sealed snapshot yet)", flush=True)
        return 0

    bindings, discovery_problems = discover_bindings(REPO_ROOT.parent / "runs")
    problems = discovery_problems + check(rows, bindings, require_bindings=True)
    if problems:
        print("SNAPSHOT IMMUTABILITY: FAIL", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 1
    roots = sum(len(value) for value in bindings.values())
    print(f"SNAPSHOT IMMUTABILITY: OK ({len(rows)} snapshot(s), {roots} complete root binding(s))", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
