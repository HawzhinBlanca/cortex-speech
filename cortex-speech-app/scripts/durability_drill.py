"""Kill/restart durability drill (Week-1 reliability item) — a repeatable, scriptable crash storm.

N cycles of: spawn the REAL writer binary (src-tauri/src/bin/durability_writer.rs — production
Database::open_with_retry + db.initialize(), the exact app boot sequence, then production
insert_segment writes), hard-kill it, restart, and verify invariants:

  1. PRAGMA integrity_check == ok           (no corruption)
  2. every journaled id exists in the DB    (zero LOST journaled edits — the writer journals+flushes
                                             an id only AFTER its DB commit returned)
  3. the drill id space is CONTIGUOUS       (0..max with no holes — a hole would mean a committed row
                                             vanished or a resume regression silently UPSERT-clobbered
                                             one; note `id` is the PRIMARY KEY, so "duplicate rows" are
                                             schema-impossible and deliberately NOT claimed)
  4. committed-but-unjournaled tail is bounded (≤1 per kill since the last verify — the only legal gap,
                                             a kill landing between commit and journal append)
  5. row count never decreases              (restart never eats prior data)

Kill scheduling mixes two modes:
  - WRITE-phase kills (default): adaptively wait until journal.txt grows this cycle — the writer is
    provably past boot and mid-write — then kill after a short random delay.
  - BOOT-phase kills (every 5th cycle): kill 0.05–0.5s after spawn, most of which lands inside
    open_with_retry's integrity gate / initialize() / migrations — the recovery surface the adaptive
    gate would otherwise never test.

Verification runs on EVEN cycles and the final cycle only. On skipped cycles the python verifier does
NOT touch the database, so the NEXT writer boot performs the crashed-WAL recovery itself — putting
rusqlite's (the app's) own WAL replay on the hook, not just python's SQLite. (When python verifies, its
plain open also WAL-recovers, like any boot would.)

Scope honesty: hard process kill (TerminateProcess) proves PROCESS-crash durability only — WAL contents
survive a process kill regardless of fsync timing, so this does NOT simulate power loss.

Usage (never touches %APPDATA%\\cortex-speech — refuses a profile outside TEMP unless --force-profile):
  # build the writer first:
  #   cargo build --bin durability_writer --manifest-path src-tauri/Cargo.toml
  python scripts/durability_drill.py --exe <path-to-durability_writer.exe> [--cycles 25] [--keep]
"""

from __future__ import annotations

import argparse
import os
import random
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def verify(profile: Path, prev_count: int, prev_tail: int, kills_since_verify: int) -> tuple[int, int]:
    """Assert the invariants; return (row_count, journaled_count)."""
    journal_path = profile / "journal.txt"
    journaled = journal_path.read_text(encoding="utf-8").split() if journal_path.exists() else []

    db_path = profile / "cortex-speech.db"
    assert db_path.exists(), "database file missing after kill"
    # A plain open performs WAL recovery exactly like a boot would.
    conn = sqlite3.connect(str(db_path))
    try:
        (integrity,) = conn.execute("PRAGMA integrity_check").fetchone()
        assert integrity == "ok", f"integrity_check failed: {integrity}"
        table = conn.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='speech_segments'").fetchone()
        if table is None:
            # Kill landed during first-boot schema creation/migrations. Legal ONLY while nothing was
            # ever committed (migrations are transactional — migrations::tests::failed_migration_is_
            # all_or_nothing); with journaled edits present this is catastrophic loss.
            assert not journaled, f"table gone but {len(journaled)} committed edits journaled — catastrophic loss"
            return 0, 0
        rows = [r[0] for r in conn.execute("SELECT id FROM speech_segments WHERE id LIKE 'drill-%'")]
    finally:
        conn.close()

    lost = set(journaled) - set(rows)
    assert not lost, f"LOST journaled edits (committed but absent from DB): {sorted(lost)[:5]}"

    # Contiguity: sequential writer + append-only history means the id space must be exactly 0..max.
    # A hole = a committed row vanished (or a resume regression UPSERT-clobbered the sequence).
    seqs = sorted(int(i.removeprefix("drill-")) for i in rows)
    if seqs:
        assert seqs == list(range(seqs[-1] + 1)), (
            f"HOLES in the drill id space: {len(seqs)} rows but max seq {seqs[-1]} "
            f"(first gap near {next((k for k, v in enumerate(seqs) if v != k), '?')})"
        )

    # The only legal journal/db divergence: a kill between commit and journal append — at most one NEW
    # unjournaled row per kill. The tail is CUMULATIVE across the run (old unjournaled rows are never
    # back-filled; resume just skips past them), so bound its GROWTH per interval, not its size.
    unjournaled = len(rows) - len(journaled)
    assert unjournaled >= prev_tail, f"unjournaled tail shrank {prev_tail} -> {unjournaled} (journal rewrote history?)"
    assert unjournaled - prev_tail <= kills_since_verify, (
        f"unjournaled tail grew by {unjournaled - prev_tail} across {kills_since_verify} kill(s) — "
        "more than one commit-vs-journal gap per kill is impossible"
    )

    assert len(rows) >= prev_count, f"row count went BACKWARDS: {prev_count} -> {len(rows)}"
    return len(rows), len(journaled)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--exe", required=True, help="path to the built durability_writer binary")
    ap.add_argument("--cycles", type=int, default=25)
    ap.add_argument("--min-delay", type=float, default=0.15)
    ap.add_argument("--max-delay", type=float, default=0.60)
    ap.add_argument("--profile", default=None, help="disposable data dir (default: fresh dir under TEMP)")
    ap.add_argument("--force-profile", action="store_true", help="allow a profile outside TEMP (danger)")
    ap.add_argument("--keep", action="store_true", help="keep the profile dir for inspection")
    args = ap.parse_args()

    exe = Path(args.exe)
    if not exe.is_file():
        print(f"FAIL: writer binary not found: {exe}", flush=True)
        return 1

    profile = Path(args.profile) if args.profile else Path(tempfile.mkdtemp(prefix="cortex-drill-"))
    tmp_root = Path(tempfile.gettempdir()).resolve()
    if not args.force_profile and tmp_root not in profile.resolve().parents and profile.resolve() != tmp_root:
        print(f"FAIL: refusing profile outside TEMP without --force-profile: {profile}", flush=True)
        return 1
    profile.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ, CORTEX_APP_DATA_DIR=str(profile))
    journal_path = profile / "journal.txt"
    prev_count = 0
    prev_tail = 0
    kills_since_verify = 0
    boot_kills = 0
    write_kills = 0
    print(f"drill: {args.cycles} kill cycles against {profile}", flush=True)

    for cycle in range(1, args.cycles + 1):
        boot_phase_kill = cycle % 5 == 0
        size_at_start = journal_path.stat().st_size if journal_path.exists() else 0
        proc = subprocess.Popen([str(exe)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

        if boot_phase_kill:
            # Kill early — most land inside open_with_retry/initialize()/migrations (the boot surface).
            time.sleep(random.uniform(0.05, 0.5))
            boot_kills += 1
        else:
            # ADAPTIVE write-phase kill: wait until the journal GROWS this cycle (provably mid-write).
            # A fixed delay landed every kill inside the debug-build boot phase (observed 25/25 cycles
            # with zero commits), which tests boot-kill, not write-kill.
            deadline = time.monotonic() + 60.0
            while time.monotonic() < deadline and proc.poll() is None:
                if (journal_path.stat().st_size if journal_path.exists() else 0) > size_at_start:
                    break
                time.sleep(0.02)
            else:
                if proc.poll() is not None:
                    _, stderr = proc.communicate(timeout=10)
                    print(f"FAIL at cycle {cycle}: writer exited before writing: {stderr.decode(errors='replace')[:300]}", flush=True)
                else:
                    proc.kill()
                    proc.communicate(timeout=30)
                    print(f"FAIL at cycle {cycle}: writer produced no committed write within 60s", flush=True)
                print(f"profile kept for inspection: {profile}", flush=True)
                return 1
            time.sleep(random.uniform(args.min_delay, args.max_delay))
            write_kills += 1

        proc.kill()  # hard TerminateProcess
        _, stderr = proc.communicate(timeout=30)
        kills_since_verify += 1
        # Any stderr is a real writer complaint (kill itself produces none) — surface it.
        text = stderr.decode(errors="replace").strip()
        if text and "CORTEX_APP_DATA_DIR" not in text:
            print(f"  cycle {cycle}: writer stderr before kill: {text[:200]}", flush=True)

        # Verify on even cycles + the final cycle. Skipped cycles leave the crashed WAL untouched, so
        # the NEXT writer boot performs the recovery itself — rusqlite's replay on the hook.
        if cycle % 2 == 0 or cycle == args.cycles:
            try:
                count, journaled = verify(profile, prev_count, prev_tail, kills_since_verify)
            except AssertionError as e:
                print(f"FAIL at cycle {cycle}: {e}", flush=True)
                print(f"profile kept for inspection: {profile}", flush=True)
                return 1
            kind = "boot-kill " if boot_phase_kill else "write-kill"
            print(f"  cycle {cycle:3} [{kind}]: rows={count:6} (+{count - prev_count:4})  journaled={journaled:6}  integrity=ok", flush=True)
            prev_count = count
            prev_tail = count - journaled
            kills_since_verify = 0
        else:
            print(f"  cycle {cycle:3} [{'boot-kill ' if boot_phase_kill else 'write-kill'}]: verify deferred (next boot does the WAL recovery)", flush=True)

    if prev_count == 0:
        print("FAIL: zero rows committed across the whole drill — the writer never got to write", flush=True)
        return 1

    print(
        f"DURABILITY DRILL PASS: {args.cycles} hard-kill cycles ({write_kills} write-phase, {boot_kills} boot-phase), "
        f"{prev_count} rows committed, 0 journaled edits lost, contiguous id space (no holes), "
        f"integrity ok at every verify; rusqlite WAL replay exercised on deferred-verify boundaries",
        flush=True,
    )
    if not args.keep:
        import shutil

        shutil.rmtree(profile, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
