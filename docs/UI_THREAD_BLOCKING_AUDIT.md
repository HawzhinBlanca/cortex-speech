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

Count of the current split (updated as the migration lands): **48 async** (off the main thread) ·
**7 sync-but-offloaded** (safe) · **12 sync-and-blocking** (below) · the remaining ~62 sync commands
are trivial in-memory getters/setters or single-row DB reads/writes (fast, not a freeze risk).

**Migration progress (Week-1 item 2):** ✅ `search_segments` — migrated 2026-07-16 to `pub async fn`
+ `run_blocking` (mirrors `get_segments`), now in the `test_command_main_thread_policy.py` ratchet.
Row #12 below is kept for the record, struck through.

| # | command | class | severity | heaviest op (traced) | consent/key gate |
|---|---------|-------|----------|----------------------|------------------|
| 1 | `run_jury_pipeline` | cloud-net | **HIGH** | T0→T1→T2 chain; T2 = Gemini audio round-trips → `run_jury_pipeline_core_via` | cloud-LLM opt-in |
| 2 | `run_t2_for_segment` | cloud-net | **HIGH** | N≥3 Gemini audio calls → `jury::t2_listener::listen_and_judge_via` | cloud-LLM opt-in |
| 3 | `run_dpo_update` | cloud-net | **HIGH** | outbound HTTP POST, **~120 s** cap on a stalled endpoint → `jury::learning::run_dpo_update` | cloud-LLM opt-in |
| 4 | `transcribe_audio_with_scribe` | cloud-net | **HIGH** | audio decode + ElevenLabs Scribe POST → `scribe_transcribe_clip` | cloud-STT opt-in |
| 5 | `add_scribe_votes` | cloud-net | **HIGH** | per-segment decode/slice + ElevenLabs POST **loop** → `scribe_api::transcribe_wav_bytes` | cloud-STT opt-in |
| 6 | `models_download` | cloud-net | **HIGH** | synchronous **multi-hundred-MB** HTTP download → `ModelManager::download_model` | — |
| 7 | `models_download_all` | cloud-net | **HIGH** | synchronous loop of `download_model` over all missing models | — |
| 8 | `get_champion_engine_status` | subprocess | **HIGH** | spawns a WSL TCP probe, blocks ~5 s → `pipeline::probe_wsl_7b_server` | — |
| 9 | `check_external_provider` | subprocess | **HIGH** | shells out to `wsl --status`, blocks up to 10 s → `external_provider_status` | — |
| 10 | `check_agentic_readiness` | subprocess | **HIGH** | `wsl --status` (up to 10 s) + `model_manager.status()` | — |
| 11 | `get_audio_duration` | file-io | **HIGH** | spawns a decode probe thread, then **blocks on `rx.recv_timeout(30 s)`** → `audio::get_duration_ms` | — |
| ~~12~~ | ~~`search_segments`~~ ✅ migrated | db-scan | ~~HIGH~~ | ~~unbounded FTS5 `MATCH`~~ — now `async` + `run_blocking` (off the main thread) | — |
| 13 | `start_champion_engine` | subprocess | MED | `powershell … spawn()` — **detached**, so the real freeze is only process-creation latency | — |

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

1. **Cloud-net cluster (#1–#7)** — highest user-visible freeze (up to ~120 s). These already drop the
   DB lock; the remaining fix is to stop holding the *main thread* across the network wait. Group them
   because they share the jury/scribe/download helper shape.
2. **WSL status getters (#8–#10)** — small, self-contained, and polled from the UI, so migrating them
   removes recurring micro-freezes; good early wins.
3. **`get_audio_duration` (#11)** — drop the synchronous `recv_timeout`; make the command async and
   await the probe.
4. ✅ **`search_segments` (#12)** — DONE 2026-07-16: mirrored its already-migrated siblings (`async` +
   `run_blocking`). A `LIMIT`/pagination bound is a separate follow-up (a behaviour change — it would
   truncate large result sets — so it's deliberately not folded into the off-thread migration).
5. **`start_champion_engine` (#13)** — lowest priority (already near-instant).

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
