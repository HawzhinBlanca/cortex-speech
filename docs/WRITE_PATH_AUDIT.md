# Write-path audit — Week 2 "document what exists"

**Date:** 2026-07-16 · **Scope:** every SQLite connection + write site in `cortex-speech-app/src-tauri`
· **Method:** three parallel code-mappers (connections / write sites / serialization config), facts
cross-checked by hand (all claims carry `file:line` in the mapper outputs; key ones re-verified).
Companion evidence: the kill/restart durability drill (`scripts/durability_drill.py`, 30 hard-kill
cycles, 0 lost) exercises this exact stack.

## The verdict, honestly

**There is NO single app-level serialized writer — by design.** Writes flow through several
concurrent connection classes, and serialization is delegated to **SQLite itself** (WAL's single-writer
file lock + `busy_timeout=10000`, set uniformly in the one factory `Database::open`, db.rs:241-251):

| Writer class | Connection | Serialized by |
|---|---|---|
| UI/IPC commands (most writes) | THE global `Arc<Mutex<Database>>` (lib.rs:413) | the global mutex (in-process) |
| Jury chain + DPO | dedicated per-run conn (`JuryDbSource`, commands.rs:4786+) | SQLite WAL + busy_timeout |
| Import/transcribe pipeline | per-operation conns (`pipeline.rs:871` `open_db()`) | SQLite WAL + busy_timeout |
| WSL-7B batch refinement worker | own conn per run (commands.rs:3376) | SQLite WAL + busy_timeout |
| Couch phone review | own conn in the server thread (couch.rs:155), one-request-at-a-time | thread confinement + WAL |
| 10-min snapshot thread | fresh read conn per iteration (lib.rs:478) | read-only usage |
| `bin/` tools (separate processes) | own conns | `InstanceLock` (cross-process) + WAL |

Within one process, two threads **can** write simultaneously on different connections (e.g. the jury
committing a verdict on its dedicated conn while the curator saves an edit through the global mutex).
That is intentional (it's what keeps cloud calls from starving the UI), and the **logical** race is
guarded in SQL, not locks: both machine-verdict writers carry a
`human_decision IS NULL AND verdict NOT IN (human_*)` WHERE guard, so a late machine write against a
human-decided row is a 0-row no-op (jury/mod.rs:345-362, db.rs:2093-2095).

**Should we now build a single writer queue + bounded readers?** Not as a Week-2 item. SQLite already
serializes writes at the file level; the drill proves process-crash consistency; and the queue is a
core-architecture rewrite (GODMODE item 4) whose real wins are latency predictability and removing
busy-timeout cliffs — worth doing as part of the deliberate decomposition, not as a quick patch. What
Week 2 *should* fix are the concrete small gaps below.

## Gaps found (concrete, small, actionable)

1. **`bin/batch_processor` took no `InstanceLock`** despite its header claiming parity with
   `batch_importer` — it could write the live DB concurrently with the running app (WAL prevents
   corruption, but it violates the repo's own cross-process discipline and the boot-quarantine
   assumptions). **FIXED in this commit** (same 3-line lock as batch_importer).
2. **Multi-statement invariants outside transactions** (autocommit sequences that can interleave or
   crash between statements — benign-but-inconsistent state, not corruption):
   - `write_segment_verdict` = guarded UPDATE + `record_decision_verdict` INSERT (db.rs:2084, 2106) —
     2 statements, no tx: a crash between them loses the decision-log row for a written verdict.
   - jury `write_verdict` = UPDATE + decision-log + best-effort corrections INSERT (jury/mod.rs) — 3.
   - import journal `begin_import_job` = reap + INSERT + retention (db.rs:1314-1341) — 3;
     `transition_job` is read-then-update, not atomic check-and-set (db.rs:1448-1468).
   → Follow-up: wrap each family in a savepoint (the repo's existing `SAVEPOINT` helpers) — small,
   mechanical, testable. NOT done in this commit (each deserves its own gated change).
3. **No `BEGIN IMMEDIATE` anywhere** — all transactions are deferred, so a read-then-write tx upgrades
   its lock at first write and can hit `SQLITE_BUSY` at COMMIT under cross-connection pressure;
   `busy_timeout` + the savepoint-cleanup helper (db.rs:381-392) make this an error-not-corruption,
   and `eval.rs:664` documents the atomic-abort expectation. Follow-up candidate, low urgency.
4. **Stale comments claimed app-level open retries** in the jury path — the only retry is SQLite's
   busy_timeout. **FIXED in this commit** (comment + warn text).
5. **No app-level `SQLITE_BUSY` retry** at any call site — a >10s writer stall surfaces to the user as
   an error string. Acceptable for a single-user app; documented here so nobody assumes retries exist.
6. **Checkpointing** relies on SQLite's default auto-checkpoint (1000 pages) + a manual
   `db_wal_checkpoint` command; no scheduled TRUNCATE checkpoint. Fine at current scale; revisit with
   the 50k-segment target.

## Durability configuration (the facts)

- Every connection: `journal_mode=WAL, synchronous=NORMAL, foreign_keys=ON, cache_size=-64000,
  busy_timeout=10000` (db.rs:241-251). `synchronous=NORMAL` + WAL = durable against process crash
  (proven by the drill), durable-to-last-checkpoint against power loss (documented drill scope).
- Boot path (`open_with_retry`, db.rs:260-312): full `integrity_check`; genuine corruption triggers the
  destructive quarantine ONLY at boot; worker threads deliberately use plain `open` so the quarantine
  is unreachable from live threads.
- Backup/restore: SQLite online-backup API (page-level, no `-wal`/`-shm` file copying), snapshot listing
  opens read-only-no-pragmas so it can never write to a frozen snapshot (snapshot.rs:200-224); restore
  integrity-checks the source, refuses newer-schema snapshots, re-runs migrations in place, and is
  writer-fenced by `prepare_restore` (commands.rs:2796-2811).
- Savepoint discipline: batch insert/delete/merge/consensus + `record_human_decision`'s big
  `unchecked_transaction` are atomic; `release_savepoint` treats a failed RELEASE as rollback so the
  never-reopened shared connection can't be left inside a stale transaction (db.rs:381-392).

## What Week 2 should do next (in order)

1. SQLite online-backup **scheduled snapshot to a second directory + drilled restore** (the 10-min
   snapshot thread + `db_backup` exist; the second-directory config + restore drill do not).
2. Savepoint-wrap the gap-2 invariant families (one gated change each).
3. Fault drills: disk-full, DB-corruption bit-flip, missing-media, mid-export kill (extend
   `durability_drill.py`'s harness pattern).
4. DPAPI for plaintext keys in `secrets.env`.
