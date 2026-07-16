"""Mid-export kill drill (Week-2 fault drills) — proves the atomic-write design under real kills.

N cycles of: spawn the REAL export writer (src-tauri/src/bin/export_writer.rs — production
export::export_dataset to JSON on a disposable profile), hard-kill it mid-export, verify:

  1. every JOURNALED export (journaled only AFTER export_dataset returned) parses as complete JSON
     with the full expected row count — a finished export is never lost or truncated;
  2. NO final `.json` in the exports dir — journaled or not — is torn: each either parses completely
     or does not exist (atomic temp+fsync+rename in atomic_file.rs is the design under test);
  3. `.tmp*` staging debris MAY exist after a kill (by design — manifests exclude it); counted and
     reported, never failed on.

Scope honesty: process kill (TerminateProcess), like the durability drill — not power loss.

Usage (profile must live under TEMP unless --force-profile):
  cargo build --bin export_writer --manifest-path src-tauri/Cargo.toml
  python scripts/export_kill_drill.py --exe <path-to-export_writer.exe> [--cycles 15]
"""

from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import tempfile
import time
from pathlib import Path

SEED_ROWS = 400  # keep in sync with export_writer.rs


def verify(profile: Path) -> tuple[int, int, int]:
    """Assert the invariants; return (journaled_count, json_files, tmp_debris)."""
    journal_path = profile / "export_journal.txt"
    journaled = journal_path.read_text(encoding="utf-8").splitlines() if journal_path.exists() else []
    out_dir = profile / "exports"
    json_files = sorted(out_dir.glob("export_*.json")) if out_dir.exists() else []
    tmp_debris = list(out_dir.glob("*.tmp*")) if out_dir.exists() else []

    # 1. Every journaled export must exist and be complete.
    for line in journaled:
        p = Path(line.strip())
        assert p.is_file(), f"journaled export missing: {p}"

    # 2. NO final .json may be torn — parse each fully and check the row count.
    for p in json_files:
        try:
            data = json.loads(p.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError) as e:
            raise AssertionError(f"TORN final export file (atomicity broken): {p.name}: {e}") from e
        rows = data if isinstance(data, list) else data.get("segments", data)
        count = len(rows) if hasattr(rows, "__len__") else -1
        assert count == SEED_ROWS, f"{p.name}: complete file but wrong row count {count} != {SEED_ROWS}"

    return len(journaled), len(json_files), len(tmp_debris)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--exe", required=True)
    ap.add_argument("--cycles", type=int, default=15)
    ap.add_argument("--min-delay", type=float, default=0.05)
    ap.add_argument("--max-delay", type=float, default=0.35)
    ap.add_argument("--profile", default=None)
    ap.add_argument("--force-profile", action="store_true")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    exe = Path(args.exe)
    if not exe.is_file():
        print(f"FAIL: writer binary not found: {exe}", flush=True)
        return 1
    profile = Path(args.profile) if args.profile else Path(tempfile.mkdtemp(prefix="cortex-exportdrill-"))
    tmp_root = Path(tempfile.gettempdir()).resolve()
    if not args.force_profile and tmp_root not in profile.resolve().parents and profile.resolve() != tmp_root:
        print(f"FAIL: refusing profile outside TEMP without --force-profile: {profile}", flush=True)
        return 1
    profile.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ, CORTEX_APP_DATA_DIR=str(profile))
    journal_path = profile / "export_journal.txt"
    prev_journaled = 0
    print(f"export kill drill: {args.cycles} cycles against {profile}", flush=True)

    for cycle in range(1, args.cycles + 1):
        size_at_start = journal_path.stat().st_size if journal_path.exists() else 0
        proc = subprocess.Popen([str(exe)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        # Wait until at least one export completed this cycle (past seeding/boot), then kill mid-loop —
        # with a short random delay the kill frequently lands INSIDE a subsequent export write.
        deadline = time.monotonic() + 120.0
        while time.monotonic() < deadline and proc.poll() is None:
            if (journal_path.stat().st_size if journal_path.exists() else 0) > size_at_start:
                break
            time.sleep(0.02)
        else:
            _, stderr = proc.communicate(timeout=30) if proc.poll() is not None else (None, b"")
            if proc.poll() is None:
                proc.kill()
                proc.communicate(timeout=30)
            print(f"FAIL at cycle {cycle}: no completed export within 120s: {stderr.decode(errors='replace')[:300]}", flush=True)
            print(f"profile kept: {profile}", flush=True)
            return 1
        time.sleep(random.uniform(args.min_delay, args.max_delay))
        proc.kill()
        _, stderr = proc.communicate(timeout=30)
        text = stderr.decode(errors="replace").strip()
        if text and "CORTEX_APP_DATA_DIR" not in text:
            print(f"  cycle {cycle}: writer stderr: {text[:200]}", flush=True)

        try:
            journaled, files, debris = verify(profile)
        except AssertionError as e:
            print(f"FAIL at cycle {cycle}: {e}", flush=True)
            print(f"profile kept: {profile}", flush=True)
            return 1
        assert journaled >= prev_journaled
        print(f"  cycle {cycle:3}: journaled={journaled:4}  complete_json={files:4}  tmp_debris={debris}", flush=True)
        prev_journaled = journaled

    if prev_journaled == 0:
        print("FAIL: zero completed exports across the whole drill", flush=True)
        return 1
    print(
        f"EXPORT KILL DRILL PASS: {args.cycles} mid-export kills, {prev_journaled} journaled exports all "
        f"complete, zero torn final files (atomic temp+rename held)",
        flush=True,
    )
    if not args.keep:
        import shutil

        shutil.rmtree(profile, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
