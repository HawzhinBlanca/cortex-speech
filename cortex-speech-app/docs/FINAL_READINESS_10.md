# FINAL READINESS — the definitive 10/10 plan

> Written 2026-07-02, synthesized from a grounded multi-agent design pass: 3 inventory agents
> (assets on disk, code capabilities, the binding bar), 3 independent plan drafts (measurement /
> daily-driver / accuracy-ceiling), and 2 adversarial critics (honesty+feasibility, completeness).
> Supersedes the P0–P5 sketch in [DEEP_AUDIT_2026-07-02.md](DEEP_AUDIT_2026-07-02.md) as the
> execution plan. Scope: **personal daily use** (ledger §5) — code-signing, public distribution,
> and academic publishing are explicitly out.
>
> Local private paths (corpora, drives) are deliberately not written here (public repo); they are
> recorded in the session ledger context and passed to tools via env vars.

## The binding bar (C1–C9) and where we stand

| # | Criterion | Status 2026-07-02 |
|---|---|---|
| C1 | Measured-best default engine, never silently downgraded, number+CI | PARTIAL — fail-hard fixed; default 7B has no decision-grade CER |
| C2 | Failures cost seconds; kill mid-import → resume loses nothing | PARTIAL — preflight shipped; journal/resume absent |
| C3 | Keyboard-only word-precise review, **≥3× baseline, measured in-app** | PARTIAL — fast flow shipped; instrumentation absent |
| C4 | Jury auto-accept precision **≥99% on ≥500 human-decided segments**, funnel visible | OPEN — never computed |
| C5 | Fix a word once → never repeated; LOOP-0 on only after **measured over-trigger = 0** | OPEN — LOOP-0 currently ON, unmeasured |
| C6 | Every number traces to command+SHA; ledger current at every commit | PARTIAL — 108-commit gap just closed, no recurrence gate |
| C7 | One correction→retrain→promote cycle executed; new champion measurably beats old | OPEN — apparatus exists, zero data ever |
| C8 | Zero owner PII in public surface, zero hygiene exemptions | PARTIAL — tree clean; git history scrub pending (owner) |
| C9 | Clean-checkout ship-check green **and** running exe provably HEAD | PARTIAL — branch unmerged, no exe-is-HEAD assertion |

## Ground truth the plan is built on (verified this session)

- **GPU**: RTX 4090 (24 GB) visible in WSL; torch 2.8.0+cu128, CUDA available. **Retraining is
  feasible on this machine today.**
- **7B base weights (31 GB) verified present inside WSL's fairseq2 cache** (only the Windows-side
  export folder lacks them). `train_omni_7b.py` (QLoRA r=16 on q/v projections, DDP, thermal
  governor) + `eval_7b_gold.py` + the warm server all exist.
- **Common Voice 22 ckb is on this machine** (train/dev/test TSVs + audio) — an external,
  boundary-aligned, human-verified benchmark requiring zero owner review time. FLEURS ckb_iq is a
  one-time ~1–2 GB download.
- **The original training corpus manifest is on a disconnected drive**; the 300k-clip archive zips
  contain audio only (zero transcripts). No plan item may depend on them.
- **The app DB has 225 segments, 3 human decisions, and zero rows** in gold_segments /
  model_versions / eval_runs — the measurement machinery has never held data.
- Latent defects found by the sweep, fixed in M0: error text points to a nonexistent
  "Start 7B server.bat"; `cortex_7b_server.py` hardcodes its adapter path (promotion could never
  actually swap engines); the OmniASR archive SHA-256 pin is an **empty string**; `db_backup`
  exists but restore is not exposed; no log rotation; LOOP-0 enabled with min_hits=1, unmeasured.

## The keystone insight

**The binding resource is the owner's review hours — so they must never be spent twice.**
Every human decision made in the app must simultaneously produce: (a) boundary-aligned domain
gold, (b) jury ground truth, (c) LOOP-0 shadow-validation data, (d) a review-speed timing sample,
and (e) a training pair. That only works if all instrumentation lands **before** the review
marathon (M2 strictly precedes M3). A second consequence: external benchmarks that cost zero
owner hours (FLEURS, CV22) come first and give the engine decision within days.

---

## M0 · Foundations & truth debt (2–3 days, code only)

Nothing is measured, drilled, or claimed before these.

1. **One normalization, two runtimes.** Unify `sorani_normalize.py`, `normalizer.rs`, and
   `scorecard_7b.py`'s ad-hoc strip behind one spec; a committed Sorani edge-case fixture with a
   test asserting **byte-identical** Python/Rust output. *Every CER below depends on it.*
   Gate: pasted identical-output run.
2. **Metric provenance artifacts.** Every eval harness emits JSON: command, model SHA-256,
   manifest SHA-256, N, metric, CI, seed. Policy gate: any number in EVAL.md without a matching
   artifact fails. Artifacts must themselves pass the PII hygiene gate (they contain paths).
   Gate: demonstrably red on an artifact-less number, green after.
3. **Honesty hotfixes.** (a) Replace the dead ".bat" advice with a working action (M4.2's button;
   interim: correct text). (b) **Flip LOOP-0 to shadow mode** — it is currently live and
   unmeasured, which violates C5; shadow logging lands in M2. (c) Retire the flagged 59.45%
   N=66 number from EVAL.md with its explanatory note (kept in an appendix).
4. **Data safety before drills.** Auto-snapshot the DB (on start + periodically, rotating 10)
   **plus settings.json and the future champion-adapter pointer** (backup scope = all state
   stores); add the missing `db_restore` IPC + Settings restore picker; `PRAGMA integrity_check`
   on open → guided restore instead of crash. Gate (drill): corrupt a copy of the live DB,
   launch, observe detection + zero-loss restore.
5. **Observability.** Rotating file logs (size-capped), panic output captured, "last session
   crashed — view log" banner. Gate: force a panic in a dev build, observe the banner.
6. **C9 closure.** Bake the git SHA into the exe; ship-check asserts running-exe-SHA == HEAD;
   merge/push the audit branch (owner reviews first); one clean-checkout `make ship-check` run.
   Gate: pasted clean-checkout green + exe-SHA assertion demonstrated.
7. **Ledger-staleness gate.** Pre-commit/CI check failing when PROGRESS_LEDGER.md lags HEAD by
   >N commits — the 108-commit gap can never recur. Gate: demonstrated red on a stale ledger.
8. **C8 execution (owner action, scheduled now).** Run `scripts/scrub_git_history.sh`
   (CONFIRM=1) + force-push + post-scrub proof (`git log -S` on the purged strings finds
   nothing). Gate: pasted empty search across all refs.

## M1 · Instant numbers — the engine decision (week 1, zero owner review hours)

1. **Freeze two external gold sets**: FLEURS ckb_iq test (one-time download; record the actual N)
   and the local CV22 ckb test split. Frozen manifests with per-clip SHA-256. Both carry a
   permanent caveat: *training-set overlap with the 7B LoRA is unverifiable* (its manifest is on
   the offline drive) — stated, never hidden.
2. **Three-engine benchmark**: 7B (warm server) vs fine-tuned MMS-1B vs stock 300M on both sets —
   CER + WER + RTF, identical normalization (M0.1), paired bootstrap 95% CIs. Smoke 10 clips
   first; full run projected only from the measured smoke rate.
3. **Engine-decision protocol, then flip the default**: default = lowest CER with paired p<0.05
   (else non-overlapping CI); tie → lower RTF; residual tie → defer to app-gold (M3). Gate
   (user-observable): import a clip, the engine badge shows the protocol winner; decision +
   numbers in the ledger.
4. **Regression pinning**: extend the **existing** `nightly-real-audio.yml` with a frozen
   10-clip deterministic subset whose own exact CER is pinned (greedy decode is deterministic;
   gate on exact match ± measured run-to-run jitter — a full-run CI does **not** bound a subset).

## M2 · Instrument everything (week 1–2, parallel code — lands BEFORE the marathon)

1. **decision_log**: per-decision timing persisted at the `record_human_decision` path; in-app
   median s/segment. Gate: after 10 real decisions the panel shows the stored-row median.
2. **Per-segment T0/T1 verdict rows** (today only aggregates exist) so verdict↔human joins are
   possible. Gate: after one import, a verdict row per segment (query pasted).
3. **LOOP-0 shadow logging**: would-fire events recorded without mutating; over-trigger = a
   would-fire the human subsequently contradicts.
4. **Alignment at import**: background low-priority worker runs `align_segment` after each
   segment's ASR; ReviewMode's `ensureWordTimings` becomes a cache hit. Gate: fresh import →
   review immediately, word chips present, zero "Aligning words…" spinner; 100% coverage within a
   measured X minutes.
5. **Suspect-first queue** (toggle): order by escalated-verdict, correction-memory hits, duration
   outliers, stored confidence. 6. **Full session restore** (cursor, filter, queue mode).
7. **Gold plumbing**: ingest verified segments into `gold_segments`; `export_gold_eval_set`
   (JSONL + WAV clips in the trainer's schema).
   All new UI in M2/M4: CKB translations + `dir="rtl"` on Kurdish containers + axe check — exit
   condition on every item.

## M3 · The Gold Marathon (weeks 2–4, owner-paced — the only large owner cost)

The owner's normal daily use, with everything logging. **Explicit external dependencies: enough
new real audio to yield ≥500 decisions, and the owner's hours** (projected honestly from M2.1's
measured rate — not guessed).

1. **≥500 human-decided segments** (today: 3). Every decision quintuple-counts (see keystone).
2. **Review ≥3× (C3), measured without the practice confound**: counterbalanced blocks —
   alternate optimized/unoptimized flow within sessions on comparable audio — not
   first-100-vs-next-100. Gate: optimized-block median ≤ ⅓ baseline median, stored rows, pasted.
3. **Freeze app-gold at N≥300** verified boundary-correct clips → the **contamination-free
   domain number**: rerun all engines (same harness); this is the number the default engine
   ultimately answers to. Gate: CER+CI in EVAL.md with artifacts.
4. **Auto-accept precision (C4)**: precision = P(human accepts unchanged | auto-accept) with
   Wilson 95% CI, surfaced in-app with the funnel. Honest caveat: the denominator is the
   auto-accepted **subset** — reaching a tight CI at ≥99% may need well over 500 total decisions;
   report whatever the number is.
5. **LOOP-0 decision (C5)**: measured over-trigger over ≥200 decisions; re-enable only at 0,
   else fix (e.g. min_hits≥2) and re-measure.
6. **Diarization check**: owner marks speaker-turn errors on one 10-min two-speaker excerpt
   (~30 min); the measured attribution-error rate decides whether tuning is warranted.

## M4 · Bulletproof daily driver (weeks 2–3, parallel code + drills)

1. **Import journal + resume**: `import_jobs` / `import_chunks` tables; **persist VAD boundaries
   per chunk** (the resume-equality gate is invalid without them); startup "Resume / Discard"
   banner; SHA re-verify on resume. Distinct from F2's fail-hard rollback (engine failure), which
   stays. Gate (drill): force-kill at ~40% of a multi-hour import; resumed count == uninterrupted
   control run, zero duplicates. (Control run costs a second full GPU pass — budgeted.)
   Also measured during this drill: **peak RSS** for the multi-hour file (single-file ceiling).
2. **App-owned 7B server lifecycle**: `ServerSupervisor` spawns the WSL server on demand, polls
   :8799, logs per-request latency to `server_health`, restarts on consecutive failures or on
   degradation past a threshold **derived from measured session-start baselines** (the
   "server degrades over long sessions" claim gets its first real numbers here); state surfaced
   in the UI rail; captured stderr passes a key/secret-scrub test. Gates (drills): server down →
   one-click/automatic start and the import completes; `wsl --shutdown` mid-import → degraded→
   restarting within ≤15 s, import resumes via the journal.
3. **Lock hammer**: concurrent align+VAD+ASR+decode for 60 s in `cargo test` (no poisoned locks)
   **plus** a 30-min live review-while-importing session gated on instrumented
   keystroke→next-segment latency (no subjective "no freeze").
4. **Audio library safety**: startup inventory of `audio_path` targets, a relink tool for moved
   files, source-audio included in the backup story, and a **new-machine migration drill**
   (DB + audio + models + settings restored on a clean profile; observed working).
5. **Model integrity**: fill the empty OmniASR archive SHA-256 pin; store the champion adapter's
   SHA-256 in `model_versions`, verified at server start.
6. **Standing failure-drill matrix** (kill exe / kill WSL / corrupt DB / disk-full / degrade
   injection) scripted in a runbook; re-run after any pipeline/DB change; ledger entry per run.
7. **Maintenance cadence**: scheduled `cargo deny` re-audit that fails when an ignored advisory
   has an in-range upstream fix (the deny.toml triage can't go stale silently).

## M5 · The retrain moat (C7 — starts when M3 data exists; plumbing lands earlier)

*Nobody else has a Sorani ASR that learns from its owner. This is the moat.*

1. **`export_finetune_pack`**: emits exactly the trainer's manifest schema (audio_path relative,
   sentence, duration_seconds) + 16 kHz clips from human-verified segments; gold/holdout-excluded;
   deduped by (audio-hash, normalized text); the holdout-leak regression test extended to this
   artifact. Gate: pack dry-run parses with zero rejects; planted gold ID turns the leak test red.
2. **Registry-driven champion**: remove the server's hardcoded adapter path (app writes a
   champion pointer on promotion; server reads it). Gate (user-observable): after promotion +
   server restart, the engine badge names the new version.
3. **Expose the existing `gate_and_promote`** via IPC + a Promote button with the explainable
   gate verdict. Gate: a deliberately-worse adapter is visibly REFUSED with reasons; a better one
   swaps the champion.
4. **WSL disaster-recovery runbook**: pinned env requirements, fairseq2 cache rebuild procedure,
   smoke test — the 31 GB cache and cortex_env are otherwise a single point of failure.
5. **Corpus ledger**: every training pack recorded (SHA, rows, provenance tier
   owner-verified / CV22 / pseudo-label, split); audio-hash dedup across packs.
6. **Execute ONE full cycle** with a **pre-registered comparison** (one challenger vs champion on
   frozen app-gold, paired CI; no retry-until-win — repeated retrains against the same gold
   inflate false positives): accumulate ≥300 verified corrections → export pack (optionally +
   CV22 as volume, contamination-annotated) → QLoRA retrain on the 4090 (~10–30 GPU-h) →
   challenger AND champion evaluated on identical gold → import → gate → promote (or the honest
   failure is logged as the cycle's completion — **either outcome closes C7's "executed once"**;
   the *win* remains the criterion for calling C7 fully green).
   Documented end-to-end as `docs/RETRAIN_RUNBOOK.md`.

## M6 · Embedded-engine speed (independent, GPU-idle time)

**DirectML for the MMS-1B** in `wav2vec2_asr.rs` behind a setting, CPU fallback on EP failure.
Gate: measured RTF CPU vs DML on a fixed ≥10-min set **and** CER equivalence by TOST
(|ΔCER| < 0.5 pp bound — "CI includes 0" is not equivalence). A negative result (not faster /
numerically off) is shipped as the honest verdict and reverted.

## M7 · The re-audit and the honest 10/10 call

Re-score C1–C9 against pasted evidence only. Each criterion needs its measured/observed gate
green. **The 10/10 claim is made exactly when the table is all-green, and not before** — and the
re-audit entry lists any residual caveats (e.g. FLEURS contamination annotation) permanently.

---

## Explicitly cut / deferred (decided, not forgotten)

- **Sorani punctuation restoration** — real differentiator, but no C-criterion requires it and
  the spike cost is understated; revisit after 10/10 as an optional bake-off with a
  pre-registered binomial win criterion.
- **300k-clip pseudo-label corpus** — gated on a physically offline drive; serves scale, not
  daily use. If the drive returns: timeboxed 0.5-day companion-table hunt, then the measured
  pseudo-label admission protocol (100-clip owner-verified sample CER decides).
- **10k-segment stress fixture** — speculative at 44× current DB size; peak-RSS and
  responsiveness are measured on the real multi-hour drill instead; revisit when the real DB
  approaches thousands.
- Streaming/live ASR, multi-dialect, ONNX-export of the 7B, publication/leaderboard apparatus.

## Budget (honest orders of magnitude)

| Resource | M0 | M1 | M2 | M3 | M4 | M5 | M6 |
|---|---|---|---|---|---|---|---|
| Code days | 2–3 | 2–3 | 3–4 | — | 5–7 | 4–6 | 1–2 |
| Owner review hours | 0 | 0 | 0 | **the** cost (rate measured in M2, projected honestly) | drills ~2 h | ~0 (reuses M3) | 0 |
| GPU hours | 0 | ~2–4 | 0 | import passes | 1 extra full import (control run) | ~10–30 | ~1 |
| External | owner: branch review + history force-push | one-time FLEURS download | — | **new audio supply** | — | optional offline-drive recovery | ort-DML availability |

## Standing honesty protocol (unchanged, restated)

Every milestone exits through a ledger entry with the pasted command + output; bad numbers are
published as readily as good ones; no gate is weakened to pass; anything unverifiable (LoRA
training-mix overlap) is annotated forever rather than assumed away.
