# SHIP READY 100 — corrected status + task-by-task execution board

> Written 2026-07-03 from a 46-agent verified audit of the tree at `d9c084c`
> (7 parallel subsystem surveys → adversarial verification of every open blocker/major
> claim against the live tree → completeness critic). The **spec of record remains
> [FINAL_READINESS_10.md](FINAL_READINESS_10.md)** (binding bar C1–C9, milestones M0–M7);
> this board corrects execution statuses the ledger overstated and **resequences** the
> remaining work so the Gold Marathon (M3) cannot start in an unsafe or silently
> non-instrumented configuration. Scope unchanged: personal daily use; code-signing and
> distribution out of scope.

## 0. Status corrections (the honest reset — verified with file:line evidence)

The ledger entry "M2 complete (6/6)" (`d9c084c`) is **wrong against M2's own gates**.
The plan's M2 has SEVEN items; measured against the plan's user-observable gates, M2 is
roughly **2 of 7 done**:

| Item | Ledger claim | Verified reality |
|---|---|---|
| M2.1 decision timing | ✅ done | **Dead write path** — frontend `recordHumanDecision` (src/lib/commands.ts:1004–1014) never sends `timestampMs`, so `db.rs:1621`'s `if let Some(ts_ms)` never fires; `decision_log` receives **zero rows in real use**. No read path (median panel) exists. |
| M2.2 verdict rows | ✅ done | Write path real and called, but rows are written **only on jury_accept/escalated paths** (db.rs:1336–1342) — "a verdict row per segment" may be unsatisfiable by construction; the pasted-query gate never ran. |
| M2.3 LOOP-0 shadow | ✅ done | **Table only** — `loop0_shadow_log` (migration v30) has **no writer anywhere**; the marathon would collect zero shadow data for the C5 decision. |
| M2.4 background alignment | ✅ done | Code wired (pipeline.rs:1649–1667); user-observable gate (fresh import → chips, no spinner, measured coverage time) never demonstrated. |
| M2.5 suspect-first queue | ✅ done | Backend-only — `get_segments_suspect_first` (commands.rs:1289) is **not registered in the Tauri invoke_handler** and has **zero frontend callers**; unreachable from any UI. |
| M2.6 session restore | ✅ done | Cursor **written** on every decision but **never read back** — frontend `SessionState` omits the field; ReviewMode always restarts at index 0. |
| M2.7 gold plumbing | *(silently renumbered away)* | **Unimplemented** — `export_gold_eval_set` exists nowhere but the plan doc; without it the marathon's decisions never become app-gold or training packs. |

Other corrected claims:

- **"exe-is-HEAD assertion" was never built.** `GIT_SHA` is baked (build.rs:22, lib.rs:67)
  but has **zero consumers** — no IPC command, script, or ship-check step reads it. The
  lib.rs:65–66 comment describes wiring that does not exist.
- **Both binaries are stale again** (release 07-02 13:03, debug 16:21 vs the 17:01 M2
  commit): no runnable build contains M2.4–M2.6. F4 has now recurred twice.
- **M1 is unexecutable as written**: `scripts/build_fleurs_ckb_manifest.py` (runbook step 1)
  never existed in git history; the runbook feeds JSON to scorecards that parse **TSV**;
  all three scorecard scripts compute **CER only** (M1.3 requires CER+WER+RTF); CV22-on-disk
  is a doc assertion with **zero ingestion tooling** — and it is also the FLEURS-failure
  fallback.
- **Gold WER gate does not gate**: all 5 `gold_wer_eval.rs` tests are `#[ignore]`;
  `check_gold_regression` is called only by its own unit tests.
- **UI copy violates the honesty law**: "~19% CER" (src/lib/i18n/en.ts:12,
  src/lib/commands.ts:62) vs the published 21.00% [19.93, 22.04] N=900 (docs/EVAL.md:37).
- Genuinely done and verified: F2 no-silent-downgrade guard + tests, F3 tree-level PII
  (history scrub still pending, owner), M0.1–M0.3/M0.5/M0.7, keyboard-first review,
  word-level edit/play, jury diff-guard, IRT persistence, champion model **bundled** in
  the installer with a working resolution path.

## 1. Standing rules (apply to every task below)

1. **No observed gate counts unless demonstrated on a HEAD exe** — P0.2's freshness
   assertion runs first, every time.
2. Every task exits through a ledger entry with the pasted command + output. "Tests pass"
   alone never closes an item that has a user-observable or measured gate.
3. No phase starts until the prior phase's blockers are green (parallel work inside a
   phase is fine).
4. Bad numbers ship as readily as good ones.

## 2. The board

**Executor legend:** `[C]` = Claude session (code, sandbox-verifiable or Windows-gates),
`[O]` = owner action, `[O+GPU]` = owner's attended GPU time.

### P0 · Truth & rebuild (½ day, do first, in this order)

| # | Task | Gate |
|---|---|---|
| P0.1 `[C/O]` | Rebuild: `npm run build` → `cargo build --release` (or `make build-app`). | Exe mtime > newest src mtime; app launches. |
| P0.2 `[C]` | Kill F4 forever: IPC command exposing `GIT_SHA`; ship-check step asserting (a) built-exe SHA == `git rev-parse HEAD`, (b) `dist/` and exe newer than newest `src/**` + `src-tauri/src/**` mtime. | Demonstrated **red** on the stale exe, green after rebuild. |
| P0.3 `[C]` | Ledger correction entry (this doc's §0) + fix duplicated M2 blocks and the dual milestone-numbering drift in PROGRESS_LEDGER.md. | Ledger reflects verified reality. |
| P0.4 `[C]` | Honesty copy fix: "~19% CER" → measured 21.00% [19.93, 22.04] N=900 in en.ts, commands.ts, CKB locale, wav2vec2_asr.rs doc comment. | Grep for `19%` clean; i18n tests pass. |

### P1 · Make M2 actually true (1–2 days — the marathon's instrumentation must provably work)

| # | Task | Gate |
|---|---|---|
| P1.1 `[C]` | Fix M2.1 dead write: frontend sends `timestampMs`; add median read path (stats panel). | 10 real decisions → panel shows stored-row median; SELECT pasted. |
| P1.2 `[C]` | Extend `write_segment_verdict` so **every** segment gets a verdict row (C4's denominator). | One real import → `COUNT(decision_verdicts) == COUNT(segments)`; query pasted. |
| P1.3 `[C]` | Implement the LOOP-0 shadow **writer** (would-fire events from the correction path). | Forced would-fire scenario → row appears; pasted. |
| P1.4 `[C]` | M2.5 reachable: register `get_segments_suspect_first` in the invoke_handler + review-queue toggle (CKB + `dir="rtl"` + axe). | Toggle visible; order changes; escalated/low-conf first. |
| P1.5 `[C]` | M2.6 read-back: restore `selected_segment_id` (+ filter, queue mode) into ReviewMode on launch. | Kill-exe drill → reopens at same segment. |
| P1.6 `[C]` | **M2.7 gold plumbing** (the dropped item): verified-segment ingest into `gold_segments` + `export_gold_eval_set` (trainer-schema JSONL + 16 kHz clips); extend the holdout-leak regression test to it. | Dry-run parses 0 rejects; planted gold ID turns leak test red. |
| P1.7 `[C/O]` | Rebuild, then run the **M2 checklist's own observed gates** end-to-end on the HEAD exe (import → review → 10 decisions → stats visible; fresh-import word-chip coverage with measured minutes; kill-exe restore). Check the boxes in M2_INSTRUMENTATION_CHECKLIST.md. | All boxes checked with pasted evidence. |
| P1.8 `[O]` | **Immediately after P1.1 lands**: record the review-throughput **baseline** (unoptimized-flow blocks) before any further UX change. | Baseline median s/segment stored in decision_log; pasted. Without this, M3.2's 3× gate is unfalsifiable forever. |

### P2 · The engine decision — M1 made executable, then run (2–3 code days + one GPU afternoon)

| # | Task | Gate |
|---|---|---|
| P2.1 `[C/O]` | Tooling preflight: (a) 5-min disk check that CV22 ckb actually exists, paths recorded via env vars; (b) write `build_fleurs_ckb_manifest.py` emitting **TSV**; (c) write a CV22 manifest builder (none exists); (d) add **WER + RTF** to scorecard_7b.py / measure_finetuned_cer.py / scorecard_finetuned.py; (e) fix the runbook's JSON/TSV mismatch. | 10-clip smoke run of each harness completes with CER+WER+RTF. |
| P2.2 `[O+GPU]` | Three-engine benchmark (7B / fine-tuned 1B / stock 300M) on frozen FLEURS ckb_iq test + CV22 ckb test: CER+WER+RTF, identical normalization, paired bootstrap 95% CIs. Contamination statement recorded (F: manifest offline → ABSENT + permanent caveat). | 3×2 table in EVAL.md with provenance artifacts. Also yields the honest FLEURS-ckb WER beside ElevenLabs Scribe's published 32.1% (caveat: vendor figure self-contradicts on their own page; ~350-sentence split → report the CI, never headline the point). |
| P2.3 `[C]` | Apply the decision protocol → flip the default **for real**: settings default + **one-time settings migration** (`#[serde(default)]` means a default flip alone never reaches the owner's persisted settings.json) + the "Use fine-tuned model"/engine control the error message already references (SettingsPanel + adapter + IPC) + fail-hard fresh-install OOBE becomes: measured champion, zero setup. | User-observable on the owner's install: import a clip → engine badge shows the protocol winner. C1 closes. |
| P2.4 `[C]` | Regression pinning: frozen 10-clip pinned-CER subset in nightly + a **local ship-check accuracy leg for the champion engine** (today's only local gate is N=1 stock-300M). De-`#[ignore]` the gold gate path by wiring `check_gold_regression`. | Deliberately corrupted decode turns the gate red. |

### P3 · Marathon-safe daily driver (pulled forward from M4 — these are prerequisites for a SAFE M3, not parallel work)

| # | Task | Gate |
|---|---|---|
| P3.1 `[C]` | Auto-snapshot rotation (DB + settings.json + champion pointer, on start + periodic, rotating 10). The marathon's output is one SQLite file; the 7B client `shutil.copyfile`s the **live** DB+WAL during imports. | Drill: corrupt a copy → detection + zero-loss restore, pasted. |
| P3.2 `[C]` | Import journal + resume (per-segment persist with VAD boundaries; "Resume import" banner). Today segments batch-insert **only at pipeline end** (pipeline.rs:1647) — a crash 2 h into a 3 h import persists nothing. | Drill: force-kill at ~40% → resumed count == control run, zero duplicates. Peak RSS recorded (multi-hour ceiling). |
| P3.3 `[C]` | Audio-source durability: managed copy-on-import **or** content-hash relink + visible "missing audio" state (segments currently reference absolute user paths in place; missing files warn-skip). | Drill: rename a source file → playback/re-transcribe/jury behavior observed and sane. |
| P3.4 `[C]` | Model-file integrity: SHA-256 pins for the bundled fine-tuned model.onnx/vocab.json + OmniASR files, verified at engine init, fail loud; fill the empty `*_ARCHIVE_SHA256` pins. | Drill: truncate a copy of the model → loud failure, not silent garbage. |
| P3.5 `[C/O]` | WSL-server failure drills **before** long imports: kill server mid-import / `wsl --shutdown` / Windows sleep-resume. ServerSupervisor lands if drills fail. | Each drill documented; recovery ≤15 s or supervised. |
| P3.6 `[C/O]` | Export round-trip verification: export a real verified set; schema-validate against the trainer; **human spot-check ~20 clips** that WAV audio matches text and boundaries (whole-file-vs-clip was this repo's most recurring bug class; N=66 gold died of exactly boundary drift). | 20/20 clips match, pasted; any mismatch = stop-and-fix. |
| P3.7 `[C]` | Windows/Sorani path robustness: import → FTS search → export with Arabic-script filenames, spaces, >200-char paths, through the WSL path translation. | Test green in cargo/e2e. |
| P3.8 `[C]` | Retention/pruning for unbounded tables (`agent_import_reports`, `decision_verdicts`, `loop0_shadow_log`) + DB size surfaced in stats panel. | Prune policy tested; size visible. |
| P3.9 `[O]` | One measured multi-hour real-file import: peak RSS, wall time, RTF, DB growth, UI responsiveness (windowed-decode path proven end-to-end, `decode_to_pcm` returns whole files as `Vec<i16>` in several paths). | Numbers in ledger; regressions become tasks. |
| P3.10 `[O]` | Git sequencing, in this exact order: review + merge + push the audit branch (36 commits) → one clean-checkout `make ship-check` green → **then** history scrub + force-push (M0.8). Scrubbing first would rewrite SHAs under an unmerged branch. | Pasted clean-checkout green; post-scrub `git log -S` empty across refs. C8/C9 close. |

### P4 · The Gold Marathon (M3 — owner-paced, weeks; unchanged in substance)

Entry condition: **all P1 + P3.1–P3.5 gates green.** Every decision quintuple-counts
(timing, verdict truth, shadow validation, gold, training pair).

| # | Task | Gate |
|---|---|---|
| P4.1 `[O]` | ≥500 human-decided segments (today: 3). | decision_log rows. |
| P4.2 `[O]` | Review ≥3× baseline, counterbalanced blocks (baseline = P1.8). | Optimized-block median ≤ ⅓ baseline; stored rows pasted. C3. |
| P4.3 `[O]` | **Escalation-rate + T1 calibration first**: measure jury escalation on real imports; if ~everything escalates (as project memory records), calibrate `jury_t1_threshold` (still the uncalibrated 0.75 constant) from verdict↔human joins **before** trusting suspect-first ordering or C4 progress. | Calibrated threshold with data; escalation rate in ledger. |
| P4.4 `[O]` | Freeze app-gold N≥300 → rerun all engines → the contamination-free domain number for the **default** engine. | CER+CI in EVAL.md with artifacts. |
| P4.5 `[O]` | Auto-accept precision (C4) with Wilson 95% CI, funnel visible in-app. | Whatever the number is, published. |
| P4.6 `[O]` | LOOP-0 decision (C5): measured over-trigger across ≥200 decisions; re-enable only at 0. | Shadow-log query pasted. |
| P4.7 `[O]` | Diarization attribution-error check (one 10-min two-speaker excerpt). | Measured rate decides whether tuning is warranted. |

### P5 · The retrain moat (M5 — plumbing may land during P3/P4; the cycle needs P4 data)

Unchanged from FINAL_READINESS_10.md §M5: `export_finetune_pack` (dedup, gold/holdout-excluded,
leak-test extended), registry-driven champion (remove the hardcoded adapter path), Promote
button with explainable gate verdict, WSL disaster-recovery runbook, corpus ledger, then
**one pre-registered champion-vs-challenger cycle** on frozen app-gold (either outcome
closes C7 "executed once"; a win makes it fully green).

### P6 · Speed + polish (optional, GPU-idle)

M6 DirectML for the 1B with TOST CER-equivalence (|ΔCER| < 0.5 pp) — a negative result
ships as the verdict and reverts. Import ETA + per-file failure list; per-segment undo in
ReviewMode; keyboard play/pause in ReviewInbox; converge the three editing surfaces toward
one text-first editor (the Descript bar) — **informed by P4 timing data, not vibes**.

### P7 · The re-audit and the honest 10/10 call (M7)

Re-score C1–C9 against pasted evidence only, on a HEAD exe, with the P0.2 freshness gate
green. Budget the verification itself (~a day of owner machine time — previously
unbudgeted). The 10/10 claim is made exactly when the table is all-green, and not before;
residual caveats (FLEURS contamination annotation, Scribe-figure comparability) are listed
permanently.

## 3. What closes which criterion

| Criterion | Closed by |
|---|---|
| C1 measured default | P2.2 + P2.3 (+P4.4 domain number) |
| C2 failures cost seconds | P3.1 + P3.2 + P3.5 |
| C3 ≥3× review, measured | P1.1 + P1.8 + P4.2 |
| C4 auto-accept precision | P1.2 + P4.3 + P4.5 |
| C5 LOOP-0 | P1.3 + P4.6 |
| C6 numbers trace | P0.3 + P2.4 (+standing gates) |
| C7 retrain cycle | P5 |
| C8 zero PII | P3.10 (scrub) |
| C9 exe provably HEAD | P0.2 + P3.10 (clean checkout) |

## 4. Top risks this board explicitly guards (from the adversarial pass)

1. **Silent instrumentation failure during the marathon** — P1 exists because M2.1 was
   dead code, M2.3 had no writer, and M2.7 didn't exist; weeks of owner labor could have
   produced zero data while everything looked green.
2. **Stale-exe recurrence invalidating observed gates** — P0.2; it has already happened twice.
3. **Data loss of the marathon's irreplaceable output** — P3.1/P3.2 are hard blockers, not
   "optional polish".
4. **M1 decision built on sand** — P2.1 verifies CV22 exists and makes the harnesses
   compute what the protocol needs *before* GPU time is spent.
5. **Accuracy ungated during code work** — P2.4 gives ship-check a real champion-engine leg.
6. **Ledger inflation** — P0.3 plus the standing rule: no gate closes on unit tests alone.
