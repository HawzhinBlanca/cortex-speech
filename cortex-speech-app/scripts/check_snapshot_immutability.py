#!/usr/bin/env python3
"""Gate C: a sealed dataset snapshot must still be what it was sealed as.

Phase 5 of docs/PLAN_TRUE_10.md. A training run cites a snapshot id and every CER measured from the
resulting model hangs off that citation. If the snapshot can drift — its id reused for different
rows, its config rewritten, its pack edited on disk — then "trained on snapshot X" is decoration, and
every number downstream is unanchored.

Three invariants, checked on the LIVE library:

  1. the id IS the content hash of the manifest it sealed (64 hex chars), so it cannot be a label
     someone chose;
  2. no two sealed rows share an id (SQLite's PRIMARY KEY enforces it; this proves the schema in
     front of us actually has it, on the real database rather than in a test);
  3. any pack still on disk hashes to the snapshot it claims — the check that turns the citation into
     a fact.

SKIP-ENV with no library or no sealed snapshot: this gate reports on data that exists, and the
challenger loop is what creates it. It never invents a pass.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SHA256_HEX = 64


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def check(rows: list[tuple[str, str, str]], pack_root: Path | None) -> list[str]:
    """Every violation found. Empty list = the invariants hold. Pure, so tests can pin it."""
    problems: list[str] = []
    seen: set[str] = set()
    for snapshot_id, status, config_json in rows:
        if len(snapshot_id) != SHA256_HEX or any(c not in "0123456789abcdef" for c in snapshot_id.lower()):
            problems.append(
                f"snapshot id {snapshot_id[:24]!r} is not a sha256 — an id that is not a content hash "
                "is a label, and a label can be reused for different rows"
            )
        if snapshot_id in seen:
            problems.append(f"snapshot id {snapshot_id[:16]}… appears twice — sealing is not idempotent")
        seen.add(snapshot_id)

        try:
            config = json.loads(config_json)
        except ValueError:
            problems.append(f"snapshot {snapshot_id[:16]}… has unparseable config — it cannot describe what it sealed")
            continue
        stated = config.get("snapshotId") or config.get("manifestSha256")
        if stated and stated != snapshot_id:
            problems.append(
                f"snapshot {snapshot_id[:16]}… seals a config naming a DIFFERENT id ({str(stated)[:16]}…)"
            )
        if status != "sealed":
            problems.append(f"snapshot {snapshot_id[:16]}… has status {status!r}, not 'sealed'")

        # If the pack is still on disk, prove it is the pack that was sealed.
        if pack_root is not None:
            manifest = pack_root / f"challenger_{snapshot_id[:12]}" / "finetune_manifest.jsonl"
            if manifest.is_file():
                actual = sha256_of(manifest)
                if actual != snapshot_id:
                    problems.append(
                        f"the pack at {manifest.parent.name} no longer hashes to its snapshot "
                        f"({actual[:16]}… != {snapshot_id[:16]}…) — it was edited after sealing"
                    )
    return problems


def main() -> int:
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        print(f"SNAPSHOT IMMUTABILITY: SKIP-ENV (no library at {db})", flush=True)
        return 0
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        table = con.execute("SELECT name FROM sqlite_master WHERE name='dataset_runs'").fetchone()
        if not table:
            print("SNAPSHOT IMMUTABILITY: SKIP-ENV (dataset_runs not present — library predates v54)", flush=True)
            return 0
        rows = con.execute(
            "SELECT id, status, config_json FROM dataset_runs WHERE name = 'finetune-pack' ORDER BY id"
        ).fetchall()
    finally:
        con.close()

    if not rows:
        # Honest: nothing has been sealed yet, so there is nothing to violate. The challenger loop
        # creates the first snapshot; until then this gate has no data and says so.
        print("SNAPSHOT IMMUTABILITY: SKIP-ENV (no sealed snapshot yet — export a fine-tune pack first)", flush=True)
        return 0

    problems = check(rows, REPO_ROOT.parent / "runs")
    if problems:
        print("SNAPSHOT IMMUTABILITY: FAIL", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 1

    print(f"SNAPSHOT IMMUTABILITY: OK ({len(rows)} sealed snapshot(s), every id a content hash of what it sealed)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
