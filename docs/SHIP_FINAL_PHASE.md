# Final Phase — Ship-Ready, One-User, Fully-Working App

**Written 2026-07-24 at HEAD `a9a6287`, after the roadmap run closed every non-owner-gated
Tier-0/1/2 defect + the T1/T2 structural test gaps (see PROGRESS_LEDGER.md iters 178–185).**

Ship = **personal use** (owner decision 2026-07-10): the app ships to the owner's own machine for
daily use. The bar is NOT lowered — ship-ready means a truly reliable, bug-free app; every honesty,
privacy, reliability, and correctness gate stays mandatory. Distribution (installer signing, stores,
updater, macOS) is out of scope and never blocks ship.

**The one-sentence status:** all of this session's fixes exist ONLY in source — the shipped exe is
**334 commits stale** (baked SHA `6f8fe3c`, built Jul 16; `scripts/check_exe_freshness.py` = FAIL).
The final phase is therefore: **build → migrate → validate live → measure → done-gate**, in that
order. Phases A and B make the app fully working; Phase C makes its claims honest; Phase D is
optional hardening. Nothing below invents a number — every metric comes from a real run.

---

## Phase A — Put the fixes into the actual app (critical path; ~1 evening)

Everything else depends on this. Until A completes, the owner is running a build that predates the
H3 provenance fix, the v41/v42 migrations, the restore reservation gate, the load-retry breakers,
and all the UI clobber/error fixes.

**A1. Rebuild the release app.**
```bash
make build-app
```
(fresh frontend `npm run build` + `cargo build --release`). Then the freshness gate must PASS:
```bash
python cortex-speech-app/scripts/check_exe_freshness.py
```
Done when it reports the baked SHA == git HEAD and no source newer than the exe.

**A2. Back up the live library BEFORE first launch.** The first launch applies schema migrations
v41 (`denoised`/`diarized`) and v42 (`vad_backend`) to the real database. They are plain nullable
`ADD COLUMN`s (fail-safe, transactional, adversarially verified) — but a restore point is
non-negotiable before any schema change on the only copy of months of review work. Use the app's
own backup (Settings → Backup, or the `db_backup` command) into a dated folder; confirm the
snapshot file exists and is non-trivial in size before proceeding.

**A3. First launch + migration check.** Launch the new exe once. Confirm: the app opens (no
newer-schema refusal, no fatal dialog), the library loads with the same segment count as before,
and a spot-check segment still plays + shows its transcript. Any failure here → restore the A2
backup, file the error, stop.

**A4. Provenance smoke test (proves the session's headline fix live).**
1. Import ONE real audio file. New segments must carry `denoised`/`diarized`/`vad_backend`
   (visible via a JSON export of that file's rows).
2. Export a dataset bundle. `manifest.json` must contain `processingProvenance`
   (`denoised`/`diarized` applied/notApplied/notRecorded counts + `vadBackend.byBackend`), with
   the pre-existing library counted as `notRecorded` (honest — those rows predate v41/v42).
3. Export the HF dataset. The README Provenance line must list the stored model id(s) of the
   written rows — not the settings dropdown value.

**Exit criteria for A:** freshness gate PASS, migration applied cleanly, smoke test shows stored
per-segment provenance end-to-end. *(~1 evening incl. the release build; estimate, not a metric.)*

---

## Phase B — Live end-to-end validation on this rig (the "fully working" proof)

The models (`src-tauri/models/`, incl. the 365 MB `mms_aligner.onnx` — present, so training-grade
alignment works) and real audio exist locally, so the full loop the CI never runs IS runnable here.

**B1. The real-app E2E driver** (the P3.4 item, executed locally instead of CI):
```bash
set CORTEX_AUDIO=<absolute path to one real clip> && npm run test:e2e:real
```
It spawns the freshly built exe over CDP, imports, transcribes, and fails on a blank transcript
(the no-fabrication guard). Done when it exits 0 and `run.jsonl` holds a real transcript.

**B2. The full user loop, driven like a user** (per `docs/COWORK_PIPELINE_PROMPT.md`): import a
directory → 300M draft → champion 7B refine via WSL (`start_champion_engine`; expect the ~8-min
warm-up — watch the status pill, don't relaunch) → review/annotate/Verify in-app → validate →
export JSONL + HF. Along the way exercise the fixed surfaces on purpose: a batch normalize while
processing (must be blocked), a restore attempt while a writer runs (must refuse), a segment edit
in ReviewMode then navigate away (no clobber, timings preserved).

**B3. The charter's single definition of done:**
```bash
make verify-10
```
`scripts/verify_10.py` is the aggregator the charter names: ship-ready is exactly
`CORTEX 10/10: ALL GATES GREEN` (exit 0). Run it, fix whatever it reports, re-run until green. If
any tier it checks is genuinely owner-gated, it will say so — that output (not this document) is
the arbiter of what remains.

**Exit criteria for B:** e2e:real exit 0, one full import→review→export cycle completed with no
defect found, `make verify-10` green (or its residue explicitly the Phase-C items below).
*(~1–2 days of driving + fixing whatever falls out; estimate.)*

---

## Phase C — Make the claims honest (owner-gated measurement work)

The app can be fully WORKING after B while its README/measurement claims are still interim. These
need the GPUs and/or the owner's judgment — surfaced all along, never faked:

- **C1. One-basis re-score (P0.1/P4.1).** Re-score all engines on ONE normalization basis on the
  frozen gold set (`make measure-10` records SHA + command + output into `docs/MEASUREMENTS.md`).
  Fan the 7B work out with concurrency = #GPUs (both 3090 Ti's busy). Until this runs, the
  cross-engine table stays annotated cross-basis — the honest interim.
- **C2. Contamination check (P4.2).** Text + audio-hash overlap between the FLEURS ckb test set and
  the 7B-LoRA / MMS training corpora. Either proves disjointness or the SOTA claim gets caveated.
- **C3. Native Sorani UI (P2.4).** The i18n wiring + parity gate are ready; what's missing is
  ~30 CORRECT technical Sorani strings (RefineryPanel, ModelRegistry, Diagnostics, shortcuts).
  Owner authors/approves them; machine-guessed Sorani stays banned.
- **C4. (Optional) Cloud judge measurement (P4.3).** Scribe v2 / current Gemini on the frozen set —
  strictly `gemini-2.5-pro` + ElevenLabs Scribe, consent-gated, never Qwen.

**Exit criteria for C:** MEASUREMENTS.md re-pinned from real runs; contamination answer recorded;
Sorani strings merged through the parity gate. *(GPU time dominates; runs unattended overnight.)*

---

## Phase D — Optional hardening (after daily use begins; skip freely)

- **D1. Chunk-overlap stitching A/B (P4.4).** It's built, tested, unused. Wire behind a flag, prove
  stitched ≥ unstitched on gold, ship only on measured non-regression.
- **D2. One-time mutation measurement (P3.3).** `cargo install cargo-mutants`, run it overnight on
  `db.rs`/`eval.rs`/the pipeline persist paths, record the real kill-rate in the ledger, and add
  tests only where a survived mutant shows a genuine hole.
- **D3. Frontend coverage floor.** Add `@vitest/coverage-v8`, measure the real baseline, set the
  threshold AT the measured number (a ratchet, not an aspiration).
- **D4. Nightly local e2e.** Schedule B1 as a nightly task on this rig so the real loop stays green
  without manual driving.

---

## Definition of DONE (ship-ready, one user)

1. `python cortex-speech-app/scripts/check_exe_freshness.py` → PASS (the running app IS HEAD).
2. `make verify-10` → `CORTEX 10/10: ALL GATES GREEN`.
3. One full real import→review→export cycle completed by the owner with zero defects.
4. `docs/MEASUREMENTS.md` re-pinned on one basis (or explicitly annotated interim), contamination
   answered, Sorani strings owner-approved.
5. Then: **use it daily for a week.** Any defect found goes through the same loop discipline
   (root-cause fix + fail-before gate + adversarial verify). A quiet week of real use is the only
   ship signal that means anything for a one-user app.

**Order matters:** A unblocks everything; B proves "fully working"; C makes it honest; D keeps it
that way. A + B are the ship gate; C is the claims gate; D is maintenance.
