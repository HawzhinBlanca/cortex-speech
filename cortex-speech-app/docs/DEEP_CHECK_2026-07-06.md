# FINAL DEEP CHECK — 2026-07-06 (117-agent adversarially-verified audit)

**Method.** 10 specialized finders swept the live worktree (absolute-path reads, main checkout
excluded as stale); every finding was attacked by 1–3 adversarial verifiers with distinct lenses
(code-truth / reachability / mitigation); a completeness critic hunted what the dimensions missed.
**61 findings confirmed** (12 blocker-severity, 19 major, 30 minor), **1 refuted**, **4 critic adds**.
Verification quality: blockers required majority of 3 independent refutation attempts.

**Live-fire evidence from the same day (not simulation):** the top blocker was confirmed against the
owner's real DB — a 17-min Halwest file imported cleanly through the 7B champion (84/84 distinct
transcripts), and minutes later **all 84 segments' `{source_start_ms, source_end_ms}` slice offsets
were destroyed** by the background aligner (verified: 0/84 intact). The transcripts survive; audio
export / playback slicing / re-transcribe for those rows silently degrade to the whole file.

## HONEST GRADE: 6.5 / 10

The substrate is genuinely strong — the clean-area list is long and real: atomic migrations with
negative tests, uniform WAL discipline, every cloud call verifiably behind its consent gate,
temp+fsync+rename settings saves, fail-closed holdout exclusion, honest provenance, 447-key i18n
parity, quarantine banner + restore path end-to-end (B2 closed), rubric-gated finetune pack (B1
closed). The prior audit's UX cluster largely landed (batch cancel, autoplay, undo, Space, queue
scoping — all verified fixed).

What keeps it at 6.5 and not higher, for a daily-driver whose product is a dataset:

1. **The primary path destroys its own data.** Background alignment permanently overwrites every
   segment's slice offsets with a bare word array (`pipeline.rs:1810-1817` writes
   `serde_json::to_string(&words)` raw instead of `chunking::merge_word_timestamps`, and aligns the
   clip's text against the WHOLE decoded file). Consequences cascade: dataset audio export pairs a
   10-s transcript with the entire recording (`export.rs:444-455` treats unparseable alignment as
   "whole file intended"); single-segment and batch re-transcribe send the whole file to the 7B
   (`cortex_7b_client.py:131-135` swallows the parse error); word timings are garbage for chunk 2..N.
   Today's deferral fix protects only the first import pass. **This is live data destruction on the
   forced-default engine's daily path.**
2. **An honestly-empty 7B transcript rolls back the entire import** (`parse_wsl_segment_result`
   treats an empty `__RESULT__` as infrastructure failure): one music/noise chunk makes a file
   permanently unimportable through the champion.
3. **The review surfaces can corrupt gold labels**: ReviewInbox overlays ReviewMode with BOTH window
   keydown handlers live (one keystroke = two decisions, one on an invisible clip); the inbox handler
   has no modifier/editable-target guard (Ctrl+A records an accept; typing in the palette fires
   decisions per keystroke); "Accept as-is" promotes an UNSEEN jury `verdict_transcript` to exported
   gold; global Ctrl+Z creates the acknowledged reverted-segment/kept-decision split; the
   `[Pending WSL 7B ASR]` placeholder renders as a normal, verifiable transcript.
4. **The verification vacuum**: no automated gate anywhere exercises the forced-default engine's
   per-segment correctness (both of today's production bugs were findable by a 2-segment fixture
   test that does not exist); the exe-freshness gate reports GREEN while the daily GUI exe predates
   the fixes (it only scans its own tree, not divergent worktrees); there is no schema
   forward-compatibility guard, so the stale pre-v32 GUI writes the v32 DB and re-plants
   `confidence=1.0` memories — reopening the exact poison path v32 closed.
5. **The C5 safety gate measures nothing**: `shadow_log_loop0` still runs inside `persist_segments`,
   BEFORE the 7B pass writes real text — every shadow observation under the default engine evaluates
   the placeholder string, so the "over-triggers must be 0" go-live gate is green on garbage.
6. **Session-end data loss**: the 1-s debounced autosave has no flush on window close — the last
   correction of every session is silently lost (no `onCloseRequested`/`beforeunload` anywhere).

None of this is polish. Items 1–3 write wrong data; 4–6 hide it. That is why the honest grade
stays at 6.5 despite an excellent substrate.

---

# THE REAL 10/10 PLAN — most robust achievable app for one daily user

Ordering principle: **stop data destruction → ship it to daily use → protect the labels → harden
ops → perfect the dataset → honest intelligence → sweep + prove.** Every fix lands with a
regression gate; a fix without a gate is incomplete. Nothing is called 10/10 until the owner-gated
re-audit (P7) inspects real output clean.

## Phase 0 — Stop the data destruction (worktree, immediately)
- **0.1 Fix the aligner write** (root cause of 9 confirmed findings): in
  `enqueue_background_alignments` — read the segment's current `alignment_json`, slice the decoded
  PCM via `slice_pcm_by_alignment` (+ `ensure_pcm_16khz`), align the SLICE, persist via
  `chunking::merge_word_timestamps(existing, &words)` through `db.update_segment_alignment_json`
  (exactly the manual path `commands.rs:1322-1336` already does right); gate on `settings.auto_align`
  (currently dead); log + count failures per import.
- **0.2 Defense in depth at every offset reader**: `cortex_7b_client.py` fails LOUD (typed exit code)
  when `alignment_json` is a list; `slice_for_export` / export audio distinguishes
  "single-chunk whole-file intended" (`chunk_count==1`) from "unparseable on a multi-chunk source"
  and refuses/flags the latter instead of exporting the whole recording.
- **0.3 Repair the damage**: repair command flags rows whose alignment is a bare array
  (offsets unrecoverable) as not-training-ready with a visible count; re-import B7876RX through the
  fixed build to regenerate its 84 segments with intact offsets.
- **0.4 Empty ≠ infra**: `parse_wsl_segment_result` returns Ok on an empty `__RESULT__` transcript
  (escalate just that segment) and Err only when no `__RESULT__` line was seen. Test both.
- **0.5 Shadow-gate integrity**: move `shadow_log_loop0` out of `persist_segments` to after
  `run_primary_wsl_pass_for_import` (both call sites — the same deferral alignment got); void
  existing placeholder-based `loop0_shadow_log` rows.
- **0.6 Regression gates for the whole class** (gates-honesty blocker): Rust test with a fake echo
  external-ASR script asserting each of 2+ segments' client calls saw DISTINCT
  `{source_start_ms, source_end_ms}`; test that `SegmentSourceMeta::from_alignment_json` still
  parses after background alignment; python policy test running the client's offset parsing against
  a word-array alignment.

## Phase 1 — Ship the fixed engine to daily use
- **1.1** Commit worktree → merge → rebuild the FULL GUI (`npm run build` first — stale-frontend
  gotcha) + `batch_importer`; run `make ship-check`; re-import B7876 (0.3).
- **1.2 Schema forward-compat guard** (critic-major): stamp the binary's max migration version; on
  open, if DB version > binary max → refuse writes with a surfaced "built by a newer version" error;
  stop swallowing `get_current_version` errors (`unwrap_or(0)`).
- **1.3 Exe-freshness gate hardening**: enumerate `git worktree list --porcelain`; fail/warn loudly
  when any worktree has uncommitted or unmerged changes under the source surfaces.
- **1.4 `make check-7b`** (local-only, folded into ship-check-local): with the server up, import a
  committed 2-segment fixture through the REAL pipeline; assert two non-empty NON-IDENTICAL
  transcripts + `omniasr-wsl-7b` provenance rows; skip loudly when WSL absent.

## Phase 2 — Protect the labels (review-surface integrity)
- **2.1** Keyboard isolation: ReviewMode's `onKeydown` returns while the inbox overlay is open;
  global KeyboardManager suspends bare-key shortcuts under any z-[100] modal.
- **2.2** Inbox `handleKey` guards: return on `ctrlKey||metaKey||altKey` and on editable targets
  (the guards ReviewMode already has).
- **2.3** Accept-what-you-see: `submit(true)` passes the displayed text
  (`recordHumanDecision(id,'accept', original)`) so `verdict_transcript` becomes what the human
  approved; render the jury proposal in ReviewMode whenever it differs from the shown draft.
- **2.4** Ctrl+Z on review surfaces remaps to the surface's own paired undo
  (clearHumanDecision + restore) — never the global history split.
- **2.5** Placeholder honesty: distinct "awaiting 7B" banner, Accept disabled (re-transcribe or
  mark-bad only), badge on cards; placeholders excluded from flat exports and counted as
  `pending_segments`; export warns/refuses while an import is running.
- **2.6** Autosave flush on close: Tauri `onCloseRequested` + `beforeunload` flush + await in-flight
  save; block close while `saveState==='saving'`.

## Phase 3 — 7B operations robustness
- **3.1** Process-wide semaphore serializing ALL 7B client spawns (import pass, batch loop,
  single re-transcribe) with queue-aware timeouts — kills the 180-s "server not running"
  misdiagnosis under concurrency.
- **3.2** Server: thread-per-connection (GPU call under an internal lock), `conn.settimeout`;
  **refuse to serve when `torch.cuda.is_available()` is False** (this deployment is defined as
  4090-backed); device banner in a health reply; preflight verifies device.
- **3.3** Contract de-triplication: app passes `CORTEX_7B_DB` / `CORTEX_7B_PORT` via env at spawn;
  client snapshots the DB via `VACUUM INTO` (atomic-consistent) instead of `shutil.copyfile` of a
  live WAL.
- **3.4** Cancel: thread the CancellationToken into `run_wsl_segment_transcript_with_script` (kill
  child ~50 ms after Cancel); define cancel semantics = roll back placeholders or journal for
  resume — a cancelled import must never duplicate segments on re-import.
- **3.5** `batch_importer` acquires the instance lock (exit with "close the app first").

## Phase 4 — Config + observability spine
- **4.1** `AppSettings::load`: strip BOM; on parse failure rename to `settings.json.corrupt-<ts>`
  (the UI's next save must never destroy the recoverable original) + surfaced startup notice.
- **4.2** Rolling file log (`tracing-appender`, data_dir/logs, keep ~5) — the release GUI currently
  discards ALL non-panic diagnostics; "Open log folder" button; crash-report surfacing on next
  launch (wire `crash_handler` check).
- **4.3** `health_check` → UI: poll on startup + interval; DiagnosticsPanel shows snapshot
  age/failure streak, free disk, effective ASR provider; notifications at thresholds.
- **4.4** Honest controls: `enable_gpu` annotated/disabled on CPU-only sherpa builds; Gemini mode
  passes the configured model to OpenRouter (no silent gpt-4o-mini substitution) and refinement
  provenance records the actual provider/model.
- **4.5** Snapshots: pruning refuses while a `cortex-speech.corrupt.*` file exists unacknowledged
  (pin pre-quarantine history); restore also restores the snapshot's `settings.json`; wire the
  existing backup command to a "Backup to folder…" button (off-disk copies).
- **4.6** `open_with_retry` (full integrity scan + destructive quarantine decision) reserved for
  boot; worker connections use plain `open` (busy_timeout already covers retry).

## Phase 5 — Dataset perfection (the product)
- **5.1** Gold reference builder uses the same preference order as training
  (`verdict_transcript → annotated → raw`, NEVER number-verbalized `normalized_transcript`) via the
  shared helper; test with an accepted digit-bearing segment.
- **5.2** Flat exports: route `training_transcript` through `canonical_training_text`; exclude
  placeholders; add composition report (per-speaker / per-source duration shares, >50% flag) to
  DatasetMetadata + HF README + pack provenance.
- **5.3** Media cache: cache the segment's SLICE (offsets are in alignment_json) instead of copying
  the entire source per playback grant; free-space check with a clear error.
- **5.4** (from 0.3) bare-array alignment rows permanently flagged not-training-ready with UI count.

## Phase 6 — Honest intelligence
- **6.1** Fine-tuned MMS juror as the 4th hypothesis in `populate_hypotheses` (breaks the 300M/1B
  kin correlation — the single biggest escalation-rate lever).
- **6.2** Evidence-loop refinements: per-slot winner-only credit in `classify_memory_outcome`
  batches (match runtime tie-break semantics); `loop0_shadow_log.segment_id` → ON DELETE SET NULL
  (survivor-bias fix); `mark_wsl_primary_unavailable` writes `agent_confidence=Some(0.0)`.
- **6.3** LOOP-0 firing provenance contract BEFORE any go-live: fired rewrites recorded
  (`rationale='loop0'` + fired memory ids in evidence_json), consistent between import and batch
  paths; C5 dashboard reads only post-7B shadow rows.
- **6.4** P2.4 closed: de-`#[ignore]` a bounded gold eval (committed-fixture pattern) feeding
  `check_gold_regression` against the baseline scorecard, model-present-gated into ship-check.

## Phase 7 — Minor sweep + proof
- **7.1** The 30 confirmed minors (directory-import cancel token into per-file processing;
  streaming path honoring its memory bound; mid-file decode error ≠ EOF; VACUUM → FTS rebuild;
  `relink_audio` stamps updated_at; EN-hardcoded undo/notification strings → i18n; nightly workflow
  honesty; Settings z-order under inbox; import_status RAII on the single-file path; torn-copy
  retry; etc. — full list in the appendix).
- **7.2** verify-10 extension: `check-7b`, schema-guard, offsets-survival, and keyboard-isolation
  gates join the suite. Then the standing owner-gated queue (P2.2 benchmark → drills → marathon →
  **P7 re-audit — the only place 10/10 may be declared**).

**Definition of done:** every confirmed finding closed with its regression gate; `make ship-check`
green including `check-7b` on the owner's machine; a re-run of this same 10-dimension adversarial
audit reports zero blockers/majors; the owner-gated P7 re-audit inspects real output clean. Until
that run, the grade stays what the evidence says it is.

---

## Appendix — confirmed findings (one line each)

### Blockers (12 confirmed; deduped root causes marked)
| # | Dim | Finding |
|---|-----|---------|
| A1 | import/db/intel/wsl/error/dataset ×7 | Background aligner replaces alignment_json with bare word array → slice offsets permanently destroyed; export/re-transcribe/playback degrade to whole file (pipeline.rs:1810-1817) |
| A2 | gates | No regression gate exercises 7B per-segment correctness; the clobber class can return green (add fixture gate) |
| A3 | gates | Exe-freshness gate green while daily exe predates worktree fixes; gate blind to worktrees (check_exe_freshness.py:163) |
| B1 | wsl | Legitimately empty 7B transcript misclassified as infra failure → whole import rolled back; file unimportable (pipeline.rs:~181) |
| C1 | frontend | ReviewInbox + ReviewMode keydown handlers both live → one keystroke records decisions on a hidden clip (ReviewInbox.svelte:375) |
| C2 | frontend | 'Accept as-is' promotes unseen jury verdict_transcript to exported gold (ReviewMode.svelte:368) |

### Majors (19 + 2 critic)
settings silent-reset on BOM/parse failure (settings.rs:429, ×2 dims); gold refs verbalized-number
poisoning (eval.rs:135); post-quarantine snapshot-rotation trap (snapshot.rs:77); open_with_retry
in hot worker paths (commands.rs:4085); no persistent log — windows_subsystem discards stdout
(lib.rs:341); health signals never reach UI (commands.ts:947); inbox no modifier/editable guard
(ReviewInbox.svelte:316); placeholder renders as normal verifiable transcript (ReviewMode.svelte:282);
global Ctrl+Z split state (App.svelte:547); 7B exercised by no gate (nightly-real-audio.yml:69);
P2.4 gold regression unwired (scorecard.rs:440); aligner aligns against whole file — garbage timings
(pipeline.rs:1810, ×2 dims); cancel mid-7B persists partial import → re-import duplicates
(pipeline.rs:1853); LOOP-0 shadow logs placeholders — C5 gate vacuous (pipeline.rs:1762); fine-tuned
juror absent (pipeline.rs:2633); 7B server single-threaded — concurrent callers misdiagnosed as
server-down (cortex_7b_server.py:112); server silent CPU fallback → false rollbacks
(cortex_7b_server.py:67); **critic:** no schema forward-compat guard — stale exe writes v32 DB,
re-plants confidence=1.0 memories (migrations/mod.rs:21); autosave never flushed on close — last
edit of every session lost (autosave.ts:46).

### Minors (30 + 2 critic — full detail in audit transcript)
batch_importer bypasses instance lock; cancel can't interrupt in-flight WSL child (300 s);
single-file import_status guard; enable_gpu dead setting (×3 dims); Gemini→OpenRouter gpt-4o-mini
mislabel; flat exports non-canonical text; no composition report; dead backup/restore IPC + false
safety comment; VACUUM/FTS desync; relink_audio updated_at; shadow-log CASCADE survivor bias;
snapshot restore skips settings.json; silent alignment failures; crash reports never surfaced;
EN-hardcoded strings in CKB; nightly fixtures branch dead; dir-import cancel between-files-only;
streaming path buffers whole file (~1.4 GB transient); mid-file decode error treated as EOF;
LOOP-0 firing provenance absent + inconsistent paths; 7B-unavailable rows at 0.5 plateau;
evidence double-credit for slot siblings; torn WAL copy in client snapshot; port/DB-path
triplication; rollback not guaranteed on every 7B error path; **critic:** media cache copies whole
source per playback grant; flat exports ship placeholder rows.

### Verified fixed (not re-reported — confirmed present in this tree)
v32 evidence-based confidence (math verified end-to-end incl. SQL posterior reconstruction);
alignment deferral at both import call sites; B1 finetune-pack rubric enforcement; B2 quarantine
banner + empty-DB snapshot guard + restore path; holdout fail-closed everywhere; consent gates on
every cloud call; settings save atomicity; autoplay/undo/Space/batch-cancel/queue-scoping UX
cluster; suspect-first uses real irt_confidence for jury rows; memory-pressure check now real;
i18n 447-key parity.

*Audit run: 117 agents, 10 find + ~103 verify + critic; adversarial majority voting on blockers;
1 claim refuted. Full machine-readable results archived in the session transcript.*
