# Cortex Speech — Owner Hand-off

**Date:** 2026-07-11 · **Branch:** `codex/newbranch` · **Author:** autonomous GODMODE loop (Claude)

## Honest status — NOT a declared 10/10

This session materially hardened the app along the reliability / durability / privacy / honesty
dimensions and closed several audit P0/P1 items **with proof-gated, adversarially-verified commits**.
It did **not** reach a genuine 10/10 — that bar has owner-gated legs (real annotators, ≥500 real review
decisions, a retrain cycle, live 7B/WSL hardware drills, real calibration data) that **cannot be faked
or produced by an agent**. Reaching this honest owner-gated stopping point is the correct outcome, not a
failure. The baseline external audit graded ≈7.2/10 (`cortex-speech-app/docs/TRUE_RATING_2026-07-09.md` /
`1010PATH.md`);
this session raised the engineering-rigor, data-durability, privacy-proof, and honesty facets, but the
accuracy / calibration / language-breadth / trust-proof dimensions remain owner-gated on real data.

`make verify-10` / `python scripts/verify_10.py` is **not fully green** — two Tier-3 legs are still
`not-built` (see below) and several legs are owner-gated. Do **not** describe the app as "10/10" until
`verify_10.py` prints all-green on a clean checkout AND the owner-gated checklist below is genuinely done.

## What this session closed (all on `codex/newbranch`, each gated + adversarially reviewed)

- **P0 #2 main-thread safety** — 46 slow commands moved off the UI thread; runtime-proven (`npm run
  test:heartbeat`, get_settings p95 ~2.3 ms under load).
- **P0 #3 Job Supervisor — COMPLETE, runtime-proven, user-visible.** migration v37 `jobs` table + pure
  `crate::jobs::JobState` machine + DB accessors (`create_or_get_job` idempotent, `transition_job`
  enforced, `update_job_progress` clamped, `run_tracked`, `mark_orphaned_running_jobs_failed` startup
  crash-recovery) + `get_jobs` IPC + a `JobsActivityPill` header surface; `export_dataset` and
  `export_huggingface_dataset` bracketed. Proven end-to-end by `npm run test:jobs`.
- **P0 #4 supervision policy/driver** — pure, fully unit-tested `engine_supervisor.rs`
  (circuit-breaker + warm-up + backoff + `engine_state_label`). **The live tokio tick loop is NOT wired
  (owner-gated — needs the 7B/WSL server to verify).**
- **P0 #5 backup/restore fencing** — restore now (a) **refuses a snapshot from a newer schema** before
  clobbering the live DB, and (b) **re-migrates an older snapshot forward to HEAD** in place; the
  undo-clear window that (b) opened was closed (history cleared on any restore that reached the swap).
  A pre-restore pinned safety snapshot already existed (`prepare_restore`).
- **P0 #9 runtime egress (SCOPED)** — `npm run test:egress` (`scripts/egress_probe.cjs`) runtime-proves
  **zero external TCP** from the backend PID on the default-offline **startup+browse** path, with an
  **in-run positive control** that fails loud if the sampler is dead (no vacuous pass). **The
  transcribe-path leg and an airtight kernel/ETW trace remain owner-gated (below).**
- **P1 data durability** — `SHA256SUMS` integrity manifest added to the audio export (the lone
  multi-file export missing it) + a root-fix to the shared `write_sha256sums` staging-file exclusion
  (`.tmp-<pid>-<nonce>` fragments were being hashed in); **multi-source SRT/VTT export refused** (#157 —
  it would ship timestamps that reset to zero at each source boundary).
- **P1 truthful intelligence** — conformal certificate now **discloses its confidence-source provenance**
  (real-posterior vs heuristic); Parquet dataset export now **ships the `alignment_quality` precision
  marker** (#8 — approximate timing was silently shipped as if precise); the "OOD detector" UI was
  **relabeled to the honest "Signal-Anomaly (heuristic)" screen** (#7 — it is a ZCR/energy heuristic,
  not a trained OOD classifier).

Key honest finding surfaced for the owner: **the T0 auto-accept gate already calibrates on the IRT
cross-model consensus confidence, NOT the heuristic 0.90 `seg.confidence`** — so the heuristic confidence
does **not** drive autonomous acceptance. It only feeds the informational dataset certificate + the
active-learning ranking. Whether to *fence* heuristic confidence out of those two is a **design decision
(below)**, deliberately NOT auto-made because the nonconformity score's real signal comes from `ctc_score`
and a naive fence could destroy a legitimate ctc-based calibration.

## Remaining work to a genuine 10/10

### [owner-gated] — needs real data / hardware / humans; cannot be faked

- **Gold Marathon** — accumulate **≥500 real human review decisions** (correction memory + LOOP-0 stay
  shadow-only until then). This is the gate for auto-accept precision, calibration, and subgroup behavior.
- **Retrain cycle** — run one complete **train → evaluate → promote/refuse → rollback** on frozen gold and
  record the outcome *even if the new model loses*. Export the finetune pack via `export_finetune_pack`
  (holdout-excluded), train off-device, re-import via `import_finetuned_model` / integrity gate.
- **Inter-annotator agreement (IAA kappa)** — recruit **≥2 independent Sorani annotators**, measure
  Cohen's/Fleiss' kappa on a shared subset. (`verify_10` `iaa-kappa-ceiling`.)
- **CORDI dialect fairness** — obtain the CORDI corpus, run the dialect-slice CER-disparity check.
- **Real calibration split** — freeze a human-gold calibration set; produce reliability diagrams, Brier
  score / ECE, selective-risk curves. Auto-accept stays shadow-only until the upper confidence bound on
  its error rate meets a ratified threshold.
- **Live 7B/WSL supervision** — wire the pure `engine_supervisor.rs` into a live tokio tick loop
  (`Arc<Mutex<SupervisionState>>` in AppState; every N s compute `healthy = probe_wsl_7b_server(3)` then
  `state.tick(healthy, elapsed)`; on `Decision::Restart` call the `start_champion_engine` launch
  mechanism — gated behind an **off-by-default settings flag** so it never surprises on the offline
  default path). Then run the **crash/restore + soak drills** against the real 7B server.
- **Egress transcribe-leg** — extend `scripts/egress_probe.cjs` to run a real local transcription (needs
  a local model + audio) so the ASR path (where cloud STT/LLM would fire if consent leaked) is covered;
  add a **kernel/ETW socket trace** for the airtight (non-poll) version. Only then flip
  `verify_10.py` `egress-runtime` from `not-built` to a hard gate.
- **Diarization** — pin + verify the CAM++ model, benchmark **DER on hand-labeled Sorani multi-speaker
  audio**, add speaker-count estimation + overlap handling; only enable by default once measured.

### [design-decision — owner] 

- **Heuristic-confidence fence** — decide whether the conformal **dataset certificate** and
  **active-learning ranking** should exclude heuristic-source confidences (making the default-path cert
  honestly "uncalibrated") or keep the current graceful `ctc_score`-based degradation. The autonomy gate
  is unaffected either way (it uses IRT). The provenance is now *disclosed* (this session) so the owner
  can see the basis before deciding.

### [verifiable-here-later] — could be done by an agent, but larger / needs coordination

- **Architecture decomposition** — extract Import/Review/Export/Models/Evaluation/Recovery/Settings
  vertical slices out of the god-files (`App.svelte` ~3.3k lines; `commands.rs`/`db.rs`/`pipeline.rs`
  >4k each). **These three `.rs` files are actively edited by the Codex agent — coordinate to avoid
  clobbering.** Preserve behavior + command names; move one slice at a time with tests.
- **`media.rs` playback cache** — `grant_source` still does a whole-file `std::fs::copy` (media.rs:~92);
  for a multi-GB audiobook that copies the entire file into the temp cache. Serve tightly-authorized
  **byte ranges** / cache only the requested segment instead. (Real proof wants a large file.)
- **IPC-contract generation** — pilot & pin `tauri-specta` (its v2 is a prerelease) to generate
  request/response/error bindings from Rust so the ~120 commands and ~118 TS wrappers can't drift.
- **Chunking overlap/dedup** — add limited acoustic overlap + timestamp/token-alignment dedup to adjacent
  ASR subchunks; **regression-test boundary words on long recordings** (the real A/B is owner-gated).
- **Per-source SRT/VTT export** — a multi-source library currently must use TXT or filter to one source
  (this session added the guard); a true per-source-file subtitle export would let it get subtitles.
- **`verify_10.py` not-built legs:** `egress-runtime` (full, see above) and `refinery-lift`
  (fixed-seed injected-error synthetic benchmark, ≥30% CER reduction at ≤15% escalation).

## How to run each local gate (Windows, from `cortex-speech-app/` unless noted)

```
# Aggregate 10/10 checker (from repo root)
python scripts/verify_10.py

# Fast sandbox-runnable gates
npm run test:python-policies      # honesty/privacy/CI/dataset policy tests (runner prints the real count)
npm test                          # vitest (frontend)
npm run typecheck                 # svelte-check + tsc
npm run lint                      # eslint

# Rust (needs the toolchain; from repo root or with --manifest-path)
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path src-tauri/Cargo.toml

# Runtime proofs against the REAL exe (need a release build; disposable profile, refuse %APPDATA%)
npm run test:heartbeat            # main-thread stays responsive under load
npm run test:jobs                 # export records a durable succeeded job (get_jobs 0->2)
npm run test:egress               # zero external TCP on the default-offline startup+browse path
npm run test:e2e:real             # real exe, real audio, real transcript (no-fabrication guard)
```

## Rebuild note (read before any live check)

The committed release exe and the frontend bundle are **stale versus HEAD** — this session changed Rust
(`db.rs`, `commands.rs`, `export*.rs`, `quality/conformal.rs`, `jobs.rs`, `migrations`, `lib.rs`) and
frontend (`ValidationPanel.svelte`, i18n, `JobsActivityPill`, `commands.ts`). Before running any live
harness or using the app, rebuild:

```
npm run build
cargo build --release --manifest-path src-tauri/Cargo.toml --bin cortex-speech-app
```

(Kill any stray instance first on Windows: `taskkill /F /IM cortex-speech-app.exe` — a locked exe makes
the link fail with `os error 32`.) The heartbeat/jobs/egress probes launch the exe themselves against a
**disposable** `CORTEX_APP_DATA_DIR` and refuse the real `%APPDATA%\cortex-speech` profile.

---

*This hand-off is the honest end of the autonomous loop: the verifiable-here surface (non-Codex,
crisp, real-value) is exhausted; everything above is owner-gated, a design decision, or larger work that
should be coordinated with the Codex agent. Restart with `/loop` any time to resume.*

---

## Resumed phase (post-hand-off, owner asked to continue) — 2026-07-11

After the hand-off the owner asked to keep going, so the loop took on the LARGER
`[verifiable-here-later]` items. Delivered (all gated + adversarially verified, on `codex/newbranch`):

- **Git hygiene** — `1010PATH.md` (the private root audit) gitignored so it can't be committed by
  accident and no longer clutters `git status`; working tree clean; all branch commit subjects verified
  well-formed.
- **P1 media playback hard-link** — `media.rs` no longer copies the whole source audio into the
  asset-protocol cache on every playback grant; it **hard-links** (instant, zero extra disk,
  byte-identical) with the original copy kept only as the cross-volume / linkless-FS fallback. Kills the
  multi-GB-copy-per-grant problem for the common same-volume case. 0-defect 2-lens adversarial review;
  new unit tests prove it links (not copies) and that pruning the cache never deletes the source.

**Then genuinely re-scanned and stopped again** — the remaining `[verifiable-here-later]` items are, on
honest inspection, either owner-gated or entangled:
- **Chunking overlap/dedup** — the pure seam-dedup logic is *speculative dead code* until chunk-overlap
  is wired into the ASR decode loop AND validated on real long recordings (owner-gated per the audit).
  Writing it now would be unvalidated guesswork; deferred to the owner.
- **Per-source SRT/VTT export** — turning the current multi-source *refusal* into per-source files is a
  real feature but carries a UX/naming design decision (how to name N output files from one save path) —
  an owner call.
- **God-file decomposition** (`commands.rs`/`db.rs`/`pipeline.rs`) — Codex-owned; must be coordinated.
- Other `fs::copy`/whole-file-read sites scanned are small config files or necessary cloud-send reads —
  not perf bugs.

Net: the clearly-high-value, non-speculative, non-Codex, verifiable-here surface is exhausted again. An
honest stop here beats manufacturing niche or unvalidated work. Restart with `/loop` any time, or point
the loop at a specific item above and it will take it on directly.

---

## Last pass (owner: "real 10/10 however possible") — 2026-07-11

A 33-agent adversarial sweep + full runtime verification pass. Every change below is committed,
gated, and adversarially refuted (one refuter finding — the settings 100 ms floor vs the
seconds-based UI round-trip — was real and fixed before commit; see the ledger).

**Where the aggregate stands now** (`python scripts/verify_10.py`, warm 7B server, CORTEX_AUDIO
set): **20 PASS, 0 FAIL** kept gates — up from 17 PASS / 2 FAIL (RED) at the start of the pass —
with 3 honest skips: `egress-runtime` (full charter leg not built; the partial startup+browse
runtime probe DID pass with a working positive control), `refinery-lift` (synthetic benchmark not
built), `fuzz-smoke` (harness now compiles — three real defects fixed — but windows-msvc cannot
link ASAN against the static-MT sherpa prebuilt; run that leg on Linux CI). Verdict:
**INCOMPLETE — green cannot be claimed** (by design while anything is skipped/owner-gated).
Reproduce with one command: `make ship-check-local`.

**Fixed this pass** (details + verbatim proofs in PROGRESS_LEDGER.md):
- Three gates could LIE and no longer can: verify_10 `--quick` printed the ship-ready GREEN while
  skipping every tier-2/3 gate (now at best INCOMPLETE); ledger-staleness and eval-provenance
  passed vacuously when their target file was missing (now hard-fail; negative-proven).
- `settings.rs`: segment-duration/thread knobs bounded at BOTH trust boundaries (update + load
  repair) — min=max=0 no longer explodes the chunk planner into one chunk per PCM sample.
- WAV/FLAC export metadata.csv now reports the duration of the clip actually written (the HF
  exporter's clamp fix, finally ported).
- Frontend gold-integrity: whole-library store loads (HIGH), ReviewMode empty-draft blanking,
  Review Inbox stale-store overwrite hazard, command-palette review-guard bypass, autosave
  pendingId() contract pinned.
- e2e harness: the "VAD produced 0 segments" failure was root-caused (disposable profile boots an
  unrunnable WSL7B default → import fail-hards before decode) and fixed — `test:e2e:real` now
  passes on the committed FLEURS fixture with the offline CTC300M engine: "REAL-DATA RUN OK".
- Runtime proofs all green on the rebuilt exe: heartbeat (p95 3.3 ms), jobs (0→2 succeeded),
  egress (zero non-loopback, positive control verified), ignored-real-model 37 gates incl. the 7B
  preflight against a genuinely warm champion server.

**Note on the 7B server:** it was started for this pass by holding a `wsl -- bash -lc "... exec
python cortex_7b_server.py"` process alive; `start_7b_server.ps1`'s nohup-detach dies when its
console exits on a NON-interactive runner (the server is killed with the launching session). From
your own interactive terminal the launcher works as designed.

**Still between here and the full-charter 10/10** (unchanged in kind, updated in count):
build the `egress-runtime` full leg and `refinery-lift` benchmark; run `fuzz-smoke` on Linux;
the 5 owner-gated legs (annotators, CORDI, Gold Marathon, branch protection, AsoSoft licensing);
the 8 owner-descoped distribution legs if scope ever widens. Nothing else verifiable-here was
found by a 33-agent sweep — 11 confirmed findings, all fixed above; 3 killed as not-real.
