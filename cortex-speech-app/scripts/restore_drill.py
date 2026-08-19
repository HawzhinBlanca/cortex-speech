#!/usr/bin/env python3
"""Destructive-restore drill: prove a snapshot can actually bring the library back.

"The backup exists" was the strongest claim available before this, and it was true while every
off-drive snapshot silently held the database ALONE (measured 2026-08-19, 11 of 11 trees). A restore
from one of those comes back with no champion pointer, and the 7B server refuses to start without a
valid schema-2 pointer — so the backup would resurrect a library that cannot transcribe.

This restores into a DISPOSABLE profile and never touches the live data directory. It simulates loss
of the primary by restoring solely from the snapshot under test, then verifies the recovered state:

  * required files present at all (DB + settings.json + champion.json)
  * manifest, when present, agrees with the bytes on disk
  * PRAGMA quick_check and foreign_key_check
  * schema version (migrations applied), so a restored DB is not silently older than the app
  * row counts for the tables that hold irreplaceable human work
  * champion identity: the registry's champion row and champion.json name the SAME deployment

Usage:
    python scripts/restore_drill.py <snapshot-dir> [--expect-fail]

`--expect-fail` inverts the verdict, so an incomplete tree can be asserted as a NEGATIVE control in
the same harness. A drill that cannot fail proves nothing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path

REQUIRED = ["cortex-speech.db", "settings.json", "champion.json"]
# Queue POLICY. Not "required" — a library legitimately has no focus, and an absent roster means
# every reviewer is unrestricted. But if the PRIMARY has one and the snapshot does not, the restore
# would silently widen who reviews what, so the drill compares against the live data dir.
POLICY = ["reviewer_dialects.json", "voice_focus.json"]
MANIFEST = "SNAPSHOT_MANIFEST.json"
HUMAN_TABLES = ["speech_segments", "review_events", "spot_checks", "model_versions"]


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def drill(snapshot: Path) -> list[str]:
    problems: list[str] = []
    with tempfile.TemporaryDirectory(prefix="cortex-restore-drill-") as raw:
        profile = Path(raw) / "cortex-speech"
        profile.mkdir(parents=True)

        # Restore SOLELY from the snapshot — nothing may be borrowed from the live directory.
        for item in snapshot.iterdir():
            if item.is_file():
                shutil.copyfile(item, profile / item.name)

        for name in REQUIRED:
            if not (profile / name).is_file():
                problems.append(f"restored profile has no {name} — recovery would be incomplete")
        live = Path(os.environ.get("APPDATA", "")) / "cortex-speech" if os.environ.get("APPDATA") else None
        for name in POLICY:
            if live and (live / name).is_file() and not (profile / name).is_file():
                problems.append(
                    f"{name} exists on the live system but NOT in this snapshot — a restore would "
                    "silently unrestrict every reviewer queue"
                )

        manifest_path = profile / MANIFEST
        if manifest_path.is_file():
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            for row in manifest.get("files", []):
                target = profile / row["path"]
                if not target.is_file():
                    problems.append(f"manifest lists {row['path']} but it is not in the tree")
                    continue
                if sha256_of(target) != row["sha256"]:
                    problems.append(f"{row['path']} does not match its manifest hash")
                if target.stat().st_size != row["sizeBytes"]:
                    problems.append(f"{row['path']} does not match its manifest size")
        else:
            problems.append(f"{MANIFEST} absent — the tree cannot prove what it contains")

        db_path = profile / "cortex-speech.db"
        if db_path.is_file():
            con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
            try:
                quick = con.execute("PRAGMA quick_check").fetchone()[0]
                if quick != "ok":
                    problems.append(f"restored DB failed quick_check: {quick}")
                fk = con.execute("PRAGMA foreign_key_check").fetchall()
                if fk:
                    problems.append(f"restored DB has {len(fk)} foreign-key violation(s)")
                # The app tracks migrations in `schema_migrations`, NOT PRAGMA user_version — reading
                # the wrong one reported "migrations did not travel" against a perfectly good snapshot.
                try:
                    version = con.execute("SELECT COALESCE(MAX(version), 0) FROM schema_migrations").fetchone()[0]
                except sqlite3.Error:
                    version = 0
                    problems.append("restored DB has no schema_migrations table — migrations did not travel")
                if not version:
                    problems.append("restored DB reports migration version 0 — migrations did not travel")
                counts = {}
                for table in HUMAN_TABLES:
                    try:
                        counts[table] = con.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                    except sqlite3.Error:
                        problems.append(f"restored DB has no {table} table")
                if counts.get("speech_segments", 0) == 0:
                    problems.append("restored DB holds zero segments — this is not a usable library")
                print(f"  schema version : {version}")
                print(f"  row counts     : {counts}")

                # Champion identity must survive the round trip, or the restored app serves nothing.
                champion_row = con.execute(
                    "SELECT id, checkpoint_sha256 FROM model_versions WHERE status='champion'"
                ).fetchone()
                pointer_file = profile / "champion.json"
                if pointer_file.is_file():
                    pointer = json.loads(pointer_file.read_text(encoding="utf-8"))
                    entry = (pointer.get("champions") or {}).get("omniasr-7b")
                    if champion_row and entry:
                        if entry.get("modelVersionId") != champion_row[0]:
                            problems.append(
                                f"champion.json names {entry.get('modelVersionId')!r} but the registry "
                                f"champion is {champion_row[0]!r}"
                            )
                        if entry.get("deploymentSha256") != champion_row[1]:
                            problems.append("champion.json deployment hash disagrees with the registry")
                        print(f"  champion       : {champion_row[0]}")
                    elif champion_row and not entry:
                        problems.append("registry has a champion but restored champion.json names none")
                    elif entry and not champion_row:
                        problems.append("champion.json names a champion the restored registry does not hold")
                    else:
                        problems.append("restored state has NO champion in either the registry or the pointer")
            finally:
                con.close()
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("--expect-fail", action="store_true", help="assert this tree is NOT restorable")
    args = parser.parse_args()
    if not args.snapshot.is_dir():
        raise SystemExit(f"not a snapshot directory: {args.snapshot}")

    print(f"RESTORE DRILL: {args.snapshot}")
    problems = drill(args.snapshot)
    for problem in problems:
        print(f"  - {problem}")

    if args.expect_fail:
        if problems:
            print(f"RESTORE DRILL: correctly REFUSED an unrestorable tree ({len(problems)} problem(s))")
            return 0
        print("RESTORE DRILL: FAILED — an incomplete tree passed, so this drill proves nothing")
        return 1
    if problems:
        print(f"RESTORE DRILL: FAILED — {len(problems)} problem(s)")
        return 1
    print("RESTORE DRILL: PASS — the library was fully recovered from this snapshot alone")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
