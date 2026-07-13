# Codex handoff — audit items that require `commands.rs` / `db.rs` / `pipeline.rs`

These are the roadmap items from the 2026-07-11 external audit (`DEBUG_cortex.md`, machine-local)
that live in **Codex-owned files** (`src-tauri/src/commands.rs`, `db.rs`, `pipeline.rs`). Claude did
the non-Codex work and specifies these precisely here so Codex can execute them cleanly. Ordered by
the audit's own execution order (safety → jobs → recovery → intelligence). Every item lists the exact
anchor, the change, and the acceptance proof.

Already shipped (do NOT redo): test isolation (`e2e_real_app.cjs`), egress proof, media hard-link
(`media.rs`), per-source SRT/VTT (`transcript_export.rs`), real CTC word aligner + honest quality
stamping (`aligner.rs` + `setup_word_aligner.py`), the align-persistence + 14-defect sweep, the
boundary-word stitch core (`chunking.rs::stitch_overlapping_transcripts`), Open/Import→Add
file/folder, actionable empty-state, and the T1 proposal-only demotion (`jury/t1_judge.rs`).

---

## P0 #2 — Move slow commands off the main thread (the audit's "most urgent discovery")

**Where:** `commands.rs` — ~120 `#[tauri::command]` fns, only ~2 async. Synchronous commands run on
the WebView main thread (Tauri v2 docs), so ASR, alignment, hashing, export, backup, model download,
cloud calls, eval, and jury processing freeze the UI. This is the same class that caused the
Open/Import freeze fixed in f01ab66.

**Change:** classify each command as (a) instant read, (b) blocking I/O, (c) CPU inference, (d)
persistent job. For (b)/(c), make the command `async` and run the body in `tauri::async_runtime::
spawn_blocking` (or a bounded `spawn_blocking` pool). `align_segment` already models this
(`commands.rs:1424` clones the pipeline out of the lock and `run_blocking(...)`) — generalize that
wrapper. Do NOT hold `state.lock_db()`/`lock_pipeline()` across the awaited body.

**Proof:** a runtime heartbeat test — spawn each slow command and assert `get_settings` + a UI tick
stay responsive (< 50 ms) throughout. Add to the integration suite.

## P0 #3 — One persistent Job Supervisor

**Where:** new `commands.rs`/service module + `pipeline.rs` orchestration. A durable `jobs` table
already exists (migration v37; `jobs.rs`). Route import, transcription, alignment, export, model
download, eval, backup, and jury through it with: a state machine, idempotency keys + step
checkpoints, bounded queues + inference concurrency, progress/ETA/cancel/retry/pause/resume, and
guaranteed task-join on shutdown (`tokio_util::sync::CancellationToken` + `TaskTracker`). Replace
every detached `std::thread::spawn` lifecycle. **Frontend seam is READY:** `ProcessingProgress.svelte`
+ the `uiStore` pipeline stores already render phase/%/elapsed/ETA/stages/cancel — a Job Center just
needs the durable job rows surfaced through the existing `get_jobs` command.

**Proof:** the audit's 100 scripted kill/restart trials → zero lost edits or duplicate segments.

## P0 #4 — App-owned 7B supervision

**Where:** `commands.rs` (`start_champion_engine`/status) + `lib.rs`. The app should start the WSL
server, wait for readiness, verify server/model/adapter hashes, restart with bounded backoff, expose
real state, terminate the process tree on shutdown, and open a circuit breaker after repeated
failures. **Non-Codex piece already done:** the server now forwards SIGTERM/SIGINT to its GPU workers
(`cortex_7b_server.py`) so a supervised restart no longer orphans replicas; the pill has a warmup
deadline (`EngineStatusPill.svelte`).

## P0 #5 — Fence backup/restore against writers

**Where:** `db.rs` backup/restore + snapshot paths. Enter an explicit maintenance state, stop new
jobs, drain writes, snapshot via SQLite's online backup API, verify, restore, reopen, run
`integrity_check`, resume. External job connections (the 7B client) must obey the same fence — note
the client reads a **file-copy snapshot** (`cortex_7b_client.py`, now WAL-retry + no `-shm` copy), so
the fence must also quiesce those.

## P1 — Replace the global DB bottleneck

**Where:** `db.rs` — the single global `Mutex<Database>`. Move to a serialized writer queue + a small
bounded read-connection pool. No network/decode/hash/inference may run while holding a DB guard
(several commands still do). Transaction boundaries belong to use cases, not IPC commands.

## P1 — Harden the schema

**Where:** `db.rs` migrations. `STRICT` tables for new/rebuilt tables, explicit CHECK + FOREIGN KEY,
transactional migrations, periodic `foreign_key_check`/`quick_check`, observable WAL checkpoint
health. Test migration from EVERY supported schema version, not just the previous one.

---

## P1 intelligence — items whose FIELD/WIRING is Codex-owned

- **OOD → "signal anomaly" rename.** The math is already honest (`quality/ood.rs` removed the
  fabricated sine-wave centroid). But the exported field is `SpeechSegment.ood_score`
  (`db.rs` struct + schema column → `oodScore` in JSON). Rename to `signal_anomaly_score`
  end-to-end (struct, column via migration, TS type, any UI label) so the published dataset never
  implies a learned in-distribution model. Claude cannot touch the `db.rs` struct/column.

- **Wire the chunk-overlap stitch.** `chunking.rs::stitch_overlapping_transcripts(prev, next,
  max_overlap_words)` is landed + unit-tested (largest-overlap-wins, orthographic-variant seam,
  no-loss). To USE it: make `plan_speech_chunks` emit a small acoustic overlap (e.g. 0.5–1.0 s)
  between adjacent chunks, and in `pipeline.rs` merge adjacent per-chunk transcripts through this
  function before persisting. Regression-test boundary words on a long recording (audit P1 #4).

- **T1 consumer.** T1 is demoted at the source (`jury/t1_judge.rs`, escalates instead of committing).
  The consumer `commands.rs:4738` still has a live `T1Decision::Commit` arm — it's now dead until the
  flag flips, but Codex may want to log/telemeter escalations to gather the "measured lift" evidence
  the audit requires before re-enabling.

## Reliability — F10 autosave vs. DB (from the 14-defect sweep)

**Where:** `commands.rs`. The frontend autosave merges edits against the Svelte STORE, which is only
reloaded on batch COMPLETION — so a curate edit during a minutes-long `batch_transcribe` can merge
against stale rows. Claude mitigated it frontend-side (editing inputs disabled while processing).
The full fix is a backend `update_segment_fields(id, partial)` IPC that reads the FRESH DB row and
applies only the changed fields under one transaction, so a partial edit can't revert concurrently-
written columns.

## Aligner model-dir inconsistency (small, real bug)

**Where:** `pipeline.rs:3146`. `ForcedAligner::new(&self.model_manager.models_dir, ...)` uses the RAW
`models_dir`, while the ASR path uses `self.model_manager.resolved_dir()`. So the aligner looks in
the unresolved app-data dir, not the resolved bundled dir — the reason `setup_word_aligner.py` has to
install into `%APPDATA%/cortex-speech/models`. Change to `resolved_dir()` for consistency (then the
aligner could ship in the bundled models dir like every other model).
