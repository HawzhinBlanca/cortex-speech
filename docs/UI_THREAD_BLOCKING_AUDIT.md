# UI-thread blocking audit — Week 1 "measure first"

**Date:** 2026-07-16 · **Scope:** `cortex-speech-app/src-tauri/src/commands.rs` (129 `#[tauri::command]`s)
· **Method:** static code trace (every sync command followed into the helper it delegates to),
cross-checked by an independent reader agent. **No wall-clock timings are claimed here — see
"Honesty" below.**

This is the Week-1 item-1 deliverable from `docs/MONTH_LOOP.md`: *identify which sync IPC commands
block the UI thread with heavy work, ranked, so the async migration (item 2) targets the worst first.*

## The rule

Tauri v2 runs a **synchronous** `#[tauri::command] fn` on the **main/UI thread**; an `async fn`
command runs on the tokio pool. A sync command that does heavy work therefore freezes the window for
the entire operation — this is the class that caused the historical Open/Import freeze.

Two traps make the naive "sync = freezes, async = safe" wrong here, so the audit is code-verified,
not keyword-guessed:

1. **Sync-but-offloaded.** Several sync commands `std::thread::spawn` the heavy body and return
   immediately, so they do *not* freeze the UI (`import_audio_file`, `resume_interrupted_import`, the
   `batch_*` family, `run_wsl_refinement`).
2. **Looks-safe-but-blocks.** `get_audio_duration` spawns a probe thread then blocks on
   `rx.recv_timeout(30s)`; the three WSL "status getters" delegate into a helper that shells out to
   `wsl` and blocks 5–10 s. A body-only marker scan sees neither block (it's behind a delegate / a
   recv), so these are traced by hand.

## Ranked worklist — sync commands that block the UI thread (migrate first)

Count of the current split (updated as the migration lands): **57 async** (off the main thread) ·
**7 sync-but-offloaded** (safe) · **3 sync-and-blocking** (below) · the remaining ~62 sync commands
are trivial in-memory getters/setters or single-row DB reads/writes (fast, not a freeze risk).

**Milestone 2026-07-16: the migration worklist is COMPLETE — the freezer worklist is now 0.** After
migrating the unwired training hook `run_dpo_update` (the last ~120 s blocking cloud POST), the one
remaining candidate — `start_champion_engine` — was **reviewed and deliberately NOT migrated**: its
body is a rate-limit check + env read + `is_file()` stat + a DETACHED `Command::spawn()` (stdio null,
`CREATE_NO_WINDOW`) that returns immediately and never waits for the child's ~8-min warm-up. Its only
UI-thread cost is process-creation latency (~ms) — below perceptibility and comparable to
`spawn_blocking`'s own dispatch overhead, so `async` would add machinery for no measurable gain.
It is reclassified as a spawn-and-return command (`OFFLOADED_HIGH` in the audit gate), where a
`.spawn()` marker pins it so a regression to a blocking `.output()`/`.status()`/`.wait()` fails the
gate. (`check_external_provider` was deleted 2026-07-16 — it was dead code.)

**Migration progress (Week-1 item 2), all 2026-07-16:**
- ✅ `search_segments` (#12) → `pub async fn` + `run_blocking` (mirrors `get_segments`).
- ✅ `get_champion_engine_status` (#8) → `async` + `run_blocking` (probe off the UI thread; infallible
  signature kept via `unwrap_or` on the unreachable JoinError).
- ✅ `check_agentic_readiness` (#10) → `async`; settings clone + bounded model-stat taken on the caller
  thread, the slow `wsl --status` probe moved to `run_blocking`.
- ✅ `get_audio_duration` (#11) → `async`; the probe-thread + 30 s `recv_timeout` watchdog now runs
  inside `run_blocking` (bound preserved, off the UI thread).
- ✅ `models_download` (#6) + `models_download_all` (#7) → `async`; the multi-hundred-MB blocking HTTP
  download (and, for `_all`, the whole missing-model loop + progress emits) moved into `run_blocking`.
- ✅ `transcribe_audio_with_scribe` (#4) → `async`; the blocking ElevenLabs Scribe upload moved into
  `run_blocking` while **every privacy gate (STT consent, DB-membership, key) stays eager** on the
  caller thread — an un-opted-in request is still rejected before any audio is offloaded.
- ✅ `run_t2_for_segment` (#2) → `async` (the Gemini "check" watcher used from ReviewMode); the
  N-sample cloud round-trip moved into `run_blocking`. The two DB phases bracket it on the caller
  thread — the brief-locked gather (audio + hyps + few-shots) drops its lock before the await, the
  verdict write re-locks after — and the `jury_cloud_opt_in` + key checks stay eager.
- ✅ `run_jury_pipeline` (#1) → `async` (the ReviewInbox "run jury" chain); `with_jury_db`'s logic was
  extracted into an owned `Send` `JuryDbSource` (dedicated-connection open inside the task, shared-handle
  fallback with lock_db's poison recovery) so the whole T0→T1→T2 chain runs on `run_blocking` —
  neither the UI thread nor the global db mutex is held across the Gemini round-trips. The
  `batch_transcribe` caller of `with_jury_db` is untouched (thin wrapper preserved).
- ✅ `add_scribe_votes` (#5) → `async` (App.svelte's batch Scribe-vote action); the per-segment
  decode/slice + POST loop moved into `run_blocking`, consent/key gates + the to-vote gather stay
  eager, and each vote's insert takes the same brief global-mutex lock as before (via `db_arc`).
- ✅ `run_dpo_update` (#3) → `pub async fn` + `run_blocking`. Still **unwired** (no `src/` caller yet),
  but it's a registered, consent-gated IPC command, so the ~120 s blocking cloud POST was a latent UI
  freeze the moment anything invoked it. Migrated pre-emptively: consent gate stays eager, the POST
  runs on the blocking pool via a separate WAL connection. This clears the last heavy freezer.
Migrated rows below are struck through.

| # | command | class | severity | heaviest op (traced) | consent/key gate |
|---|---------|-------|----------|----------------------|------------------|
| ~~1~~ | ~~`run_jury_pipeline`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~T0→T1→T2 chain~~ — now `async`; whole chain on `run_blocking` via owned `JuryDbSource` | cloud-LLM opt-in |
| ~~2~~ | ~~`run_t2_for_segment`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~N≥3 Gemini audio calls~~ — now `async`; cloud call in `run_blocking`, DB gather/write bracket it | cloud-LLM opt-in |
| ~~3~~ | ~~`run_dpo_update`~~ ✅ migrated (still unwired) | cloud-net | ~~HIGH~~ | ~~outbound HTTP POST (~120 s cap)~~ — now `async`; POST in `run_blocking` on a separate WAL connection, consent gate eager | cloud-LLM opt-in |
| ~~4~~ | ~~`transcribe_audio_with_scribe`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~ElevenLabs Scribe POST~~ — now `async` + `run_blocking` (consent gate stays eager) | cloud-STT opt-in |
| ~~5~~ | ~~`add_scribe_votes`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~decode+POST loop~~ — now `async`; loop on `run_blocking`, per-insert brief `db_arc` lock | cloud-STT opt-in |
| ~~6~~ | ~~`models_download`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~multi-hundred-MB HTTP download~~ — now `async` + `run_blocking` | — |
| ~~7~~ | ~~`models_download_all`~~ ✅ migrated | cloud-net | ~~HIGH~~ | ~~download loop~~ — now `async`; whole loop + emits in `run_blocking` | — |
| ~~8~~ | ~~`get_champion_engine_status`~~ ✅ migrated | subprocess | ~~HIGH~~ | ~~WSL TCP probe~~ — now `async` + `run_blocking` | — |
| ~~9~~ | ~~`check_external_provider`~~ 🗑️ DELETED | subprocess | ~~HIGH~~ | dead code, removed 2026-07-16 (no caller) | — |
| ~~10~~ | ~~`check_agentic_readiness`~~ ✅ migrated | subprocess | ~~HIGH~~ | ~~`wsl --status` + model stat~~ — now `async`, probe on `run_blocking` | — |
| ~~11~~ | ~~`get_audio_duration`~~ ✅ migrated | file-io | ~~HIGH~~ | ~~probe thread + `recv_timeout(30 s)`~~ — watchdog now inside `run_blocking` | — |
| ~~12~~ | ~~`search_segments`~~ ✅ migrated | db-scan | ~~HIGH~~ | ~~unbounded FTS5 `MATCH`~~ — now `async` + `run_blocking` (off the main thread) | — |
| ~~13~~ | `start_champion_engine` ✅ reviewed — spawn-and-return, NOT migrated | subprocess | ~~MED~~ | detached `Command::spawn()` (stdio null, `CREATE_NO_WINDOW`), returns without waiting for warm-up — UI cost is only process-creation latency (~ms, ≈ `spawn_blocking` dispatch), so `async` buys nothing; pinned as spawn-and-return in the gate | — |

**`search_segments` (#12) is the odd one out:** its siblings `get_segments` and
`get_segments_suspect_first` were migrated to `async` + `run_blocking`, and `get_segments_page` clamps
its `limit` to ≤ 500 — but `search_segments` runs sync on the main thread AND caps nothing, so a common
Sorani token typed into the search box scans and serialises a large fraction of the library on the UI
thread. Fired on keystroke from `SearchBar.svelte`. (Found by the adversarial completeness pass —
originally mis-filed under "bounded/secondary" below; corrected here.)

**Sneakiest freezers:** #8–#10. They read like cheap status getters the frontend polls, but each
shells out to WSL and blocks multiple seconds. #11 *looks* offloaded (it spawns a thread) yet blocks
the caller on the recv. #12 is HIGH by the subprocess-spawn rubric but its child is detached and
returns fast — listed for completeness, lowest priority of the twelve.

### Already offloaded — safe today, pinned so they stay that way

`import_audio_file`, `resume_interrupted_import`, `batch_transcribe`, `batch_verify`,
`batch_assign_speaker`, `batch_normalize`, `run_wsl_refinement` all spawn a worker thread and return
immediately. The audit gate asserts each still spawns, so a future edit can't quietly turn one back
into a UI-thread blocker.

### Bounded / secondary (not on the migrate-first list)

- `get_segments_page` — the bounded search/list DB read: paginated, limit clamped ≤ 500; low risk.
  (Contrast `search_segments` #12, which shares the query shape but caps nothing — hence it *is* on
  the worklist.)
- `start_couch_review` — binds the `tiny_http` port and hands the accept loop to its own thread
  (`couch::start`), so it returns fast; not a freezer.
- `build_scorecard` / `compute_diff` — bounded per-item compute (format an already-computed eval
  result; diff one segment's two transcripts). Not the freeze class this audit targets.

## Recommended migration order (Week-1 item 2)

Behaviour-preserving `pub async fn` + `run_blocking` (or a real async HTTP client for the network
ones), a few per run, each with a test, each added to the `test_command_main_thread_policy.py`
ratchet as it lands:

1. **Cloud-net cluster (#1–#7)** — highest user-visible freeze (up to ~120 s). ✅ The model-download
   pair (#6 `models_download`, #7 `models_download_all`) is DONE 2026-07-16 (no consent gate — just a
   blocking HTTP fetch, wrapped in `run_blocking`). Remaining #1–#5 are the jury/scribe/DPO commands:
   they already drop the DB lock; the fix is to stop holding the *main thread* across the network wait
   (a `run_blocking` wrap of the blocking client call — they're consent/key-gated, so each needs its own
   careful pass).
2. ✅ **WSL status getters (#8–#10)** — DONE 2026-07-16 for the two live ones (`get_champion_engine_status`,
   `check_agentic_readiness`): the `wsl`/probe work runs on `run_blocking`, off the polled UI thread.
   `check_external_provider` (#9) turned out **dead** (no caller) → DELETED 2026-07-16.
3. ✅ **`get_audio_duration` (#11)** — DONE 2026-07-16: the whole watchdog (probe thread + 30 s
   `recv_timeout`) moved inside `run_blocking`, so the bound is preserved but the wait is off the UI
   thread (kept the watchdog rather than dropping the timeout, which would be a behaviour change).
4. ✅ **`search_segments` (#12)** — DONE 2026-07-16: mirrored its already-migrated siblings (`async` +
   `run_blocking`). A `LIMIT`/pagination bound is a separate follow-up (a behaviour change — it would
   truncate large result sets — so it's deliberately not folded into the off-thread migration).
5. ✅ **`start_champion_engine` (#13)** — REVIEWED 2026-07-16, deliberately NOT migrated. It already
   returns immediately (detached `Command::spawn()`, no wait for warm-up); its UI-thread cost is
   process-creation latency (~ms), which is below perceptibility and ≈ the `spawn_blocking` dispatch
   overhead an `async` version would add — so migrating buys nothing measurable. Reclassified as a
   spawn-and-return command and pinned by a `.spawn()` marker in the gate. **The worklist is now 0.**

## Honesty

- **No real timings are asserted in this document.** The durations quoted (~120 s POST cap, 5–10 s WSL
  status, 30 s decode timeout) are the code's own configured *ceilings*, not measured latencies.
- **Real per-command wall-clock is owner-gated.** It requires a real run — real audio, a live model
  download, an opted-in cloud judge call — on the owner's machine. The `TRACER`
  (`src-tauri/src/telemetry/mod.rs`, 10k-span ring) already records real span durations during use and
  is surfaced by the `get_recent_spans` / `get_tracing_stats` IPC commands. Instrumenting the twelve
  freezers to emit a span each (so real timings accrue automatically) is the natural next increment.
- **Regression gates:** `scripts/test_ui_thread_blocking_audit.py` pins this list against the source
  (fails when a freezer becomes async → forces the ratchet update, so the worklist honestly shrinks);
  `scripts/test_command_main_thread_policy.py` is the forward ratchet of already-migrated commands.
