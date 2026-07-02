# Cortex Speech — Deep Audit, Honest Rating, and the Road to a True 10/10

> Written 2026-07-02 from a full read of the current tree (41.2k LOC Rust, 13.9k LOC Svelte/TS),
> four subsystem audits (frontend UX, backend reliability, intelligence stack, export/eval
> integrity), the last 108 commits, and a fresh run of every gate that runs on this machine.
> Per the charter: reality first, progress second. No number below is estimated.

## 0. Gates actually run today (2026-07-02, this machine)

| Gate | Result |
|---|---|
| `make verify-10` (root) | **GREEN** — `CORTEX 10/10: ALL GATES GREEN` (narrow manifest/license/ledger-schema gate) |
| `npm run typecheck` | **GREEN** — 393 files, 0 errors, 0 warnings |
| `npm run lint` | **GREEN** |
| `npm test` (vitest) | **GREEN** — 132/132 |
| `cargo fmt --check` | **GREEN** |
| `cargo clippy --all-targets -D warnings` | **GREEN** |
| `cargo test` | **GREEN** — exit 0, all suites pass (incl. reliability 23/23, soak 171 s, tauri_integration; real_audio 1 passed + 20 `#[ignore]` env-gated live-model harnesses) |

Green gates are **necessary, not sufficient** (readiness-plan rule #2). Everything below is about
what the gates cannot see.

## 1. Honest overall rating: **6.5 / 10** as a daily-driver product

Cortex is **category-unique** — no tool on earth does offline Sorani ASR + curation + gated export
end-to-end — and its data-integrity/honesty machinery is genuinely world-class for a solo project.
But on the owner's own bar (*reliable, fully smart, friendliest possible e2e daily tool*), it is a
6.5: the **default engine has no measured accuracy number**, a fresh/reset config **silently
downgrades to the weakest engine** (contradicting its own documented contract), the review UX is
functional-but-slow vs the historic best, the intelligence layer is sophisticated but **unmeasured
in effect**, and the public repo leaks the owner's identity in tracked files.

### Rating vs the historic top 3 (each judged on its home turf)

| Dimension | ELAN (annotation gold standard) | Descript (editing-UX gold standard) | Common Voice (dataset-pipeline gold standard) | **Cortex today** |
|---|---|---|---|---|
| Time-aligned annotation depth | 10 — arbitrary tiers, hierarchies, controlled vocab | 4 | 3 | **6** — segments + speakers + on-demand word timings, no tiers |
| Transcript-correction speed | 3 | 10 — word-click editing, text-first flow | 2 | **5** — textarea + word-click *playback only* |
| Dataset validation & export integrity | 4 | 2 | 9 — crowd votes, CC0 governance at scale | **8.5** — reject/holdout/orphan/SHA256 guards are stronger *per segment* than CV's |
| ASR quality for Sorani | n/a | ~0 (no ckb) | ~0 (collection only) | **unmatched** — fine-tuned 21.00% CER [19.93, 22.04] N=900, measured |
| Reliability as a daily tool | 9 (decades of use) | 8 | 9 (hosted) | **6.5** — hardened, but live traps (below) |
| Smart assistance that provably saves review time | 0 | 6 (good ASR + UX) | 0 | **5** — real math, zero measured effect |

**Standing:** on its niche Cortex already does things none of the three can (offline Sorani,
consent-gated biometric handling, conformal auto-accept). As a *product* it operates at ~65% of
their home-turf polish. The gap is closable — and the plan below closes it.

## 2. What is genuinely strong (earned, with evidence)

- **Honest measurement culture**: `docs/EVAL.md` numbers are real, seeded, CI'd
  (fine-tuned MMS-CTC-1B int8: micro CER 21.00% [19.93, 22.04], N=900; stock 300M: 29.40%, N=400;
  script-vs-recognition N=200 analysis). Empty-reference guard in `eval.rs:269` keeps micro rates
  honest; lift computation only counts fully-referenced segments (`eval.rs:518`).
- **Export integrity is airtight**: human-rejects excluded everywhere (`export.rs:229`,
  `export_bundle.rs:242`, commit b624339); holdout excluded by path AND content hash
  (`export.rs:187–217`); HF re-export wipes stale splits and prunes orphans before SHA256SUMS
  regeneration (`export.rs:557, 765, 890`); CSV formula-injection quoting; absolute paths never leak
  into datasets.
- **Consent gating is complete**: every cloud egress (Scribe, Gemini T2, whole-file reference,
  DPO) sits behind an explicit opt-in, audited three times, with regression tests.
- **Real engineering rigor**: 15+ confirmed defects fixed across adversarial hunts (path traversal
  CWE-22, CSV CWE-1236, lock-starvation, UB guard on the sherpa transmute), clippy `-D warnings`,
  property tests that found real bugs (FTS NUL crash), WCAG 2.2 AA axe gate enforced, anti-aliased
  sinc resampling, streaming decode for >16.6-min audio.
- **The intelligence architecture is mathematically real**: 1PL IRT consensus (`quality/irt.rs:155`),
  Hoeffding-bound conformal auto-accept with per-SNR buckets and fail-closed cold start
  (`quality/conformal.rs:56`), T2 self-consistency + swap-stable debate with anti-hallucination
  bounds (`jury/t2_listener.rs:200–228`).

## 3. The brutal list — ranked findings (all verified against the live tree)

### F1 · The DEFAULT engine has no measured accuracy number — honesty gap (CRITICAL)
`AsrModelSize::WSL7B` is the built-in default (`settings.rs:281`) and imports **fail hard** on its
absence (`pipeline.rs:1518–1598`), yet `docs/EVAL.md` contains **zero** rows for the 7B. The
measured engines (fine-tuned 1B: 21.00%; stock 300M: 29.40%) are *not* the default. The app's
primary path runs on faith — the one thing the charter forbids. Nobody can currently say whether
the forced 7B is better or worse than the embedded, measured fine-tuned engine.

### F2 · Fresh/reset settings silently downgrade to the weakest engine — the documented contract is false (CRITICAL)
`settings.rs:279` promises: *"resolved from external_asr_script_path (or the bundled client when
that is empty); when neither … available the import fails hard rather than downgrading."*
Reality: no bundled-client resolution exists anywhere. `should_use_wsl_primary_asr()`
(`pipeline.rs:453`) requires a **non-empty** script path; the default is `""` (`settings.rs:315`),
so a fresh install or lost settings.json silently transcribes with stock CTC-300M (~29–42% CER)
— the exact "silently-downgraded output the owner never asked for" the fail-hard contract exists
to prevent. Docs-contradict-code is a charter violation on its own.

### F3 · The public repo leaks the owner's identity (HIGH, privacy)
`wsl_omniasr_refine.py:196` (git-tracked, public repo) hardcodes a SQL LIKE pattern containing the
owner's first name and personal folder names; `PODCAST-002_perfect_dataset.json` and
`Technoshan_P01_perfect_dataset.json` remain tracked under a hygiene-gate exemption and carry
personal-path fragments plus the owner's private transcript content. For a project whose hardest
guardrail is "voice is biometric," the public surface should carry zero owner PII.

### F4 · The shipped app is stale right now (HIGH, operational)
`src/` newest change 2026-07-01 15:09 vs `dist/` built 06-30 02:43 and the release exe 06-30 02:49.
The July-1 fixes (human-reject exports b624339, engine badge 8c8a7c4) are **not in the app the
owner is using**. This is the known `npm run build`-before-`cargo build` trap with no gate guarding
it.

### F5 · Review speed is the product's ceiling and it is well below the Descript bar (HIGH, UX)
- Word chips are **playback-only** (`ReviewMode.svelte:485` → `playFromWord`); no click-to-edit a word.
- ReviewMode has no single-key accept/reject (ReviewInbox has A/E/X/Space; ReviewMode needs
  Ctrl+Enter or mouse).
- First open of an unaligned clip blocks on "Aligning words…" for up to ~30 s (alignment is
  on-demand, never precomputed at import — `pipeline.rs:1469` leaves word timings absent).
- No per-segment undo stack (global Ctrl+Z only), no import ETA/per-file error list.

### F6 · The 7B bridge is the most fragile link in the default path (HIGH, reliability)
No preflight health check: with the server down, a hung attempt rides a 300 s timeout
(`pipeline.rs:212`) before the fail-hard rollback fires — up to ~5 minutes of spinner to learn
"start the server." Per-segment transcription shells `wsl python3 cortex_7b_client.py` per clip
while the warm server already listens on `:8799` — process-spawn overhead + DB-copy workaround
(`wsl_omniasr_refine.py:61–87`) where a direct HTTP call from Rust with the PCM already in memory
would be simpler, faster, and testable. No import journal: a crash mid-import means re-importing
(idempotent but wasteful; jury re-runs).

### F7 · The smartness is unmeasured and partially unguarded (MEDIUM-HIGH)
- No measurement of T1 precision vs human decisions; `jury_t1_threshold` default 0.75
  (`settings.rs:266`) and lexicon 0.6 are uncalibrated constants.
- IRT model abilities are hardcoded (Gemini 2.0 … 300M −0.5, `irt.rs:33`) and EM-fitted abilities
  are never persisted — every import restarts from the priors.
- `llm_refiner.rs::refine_text` commits raw LLM output with **no diff guard** (T2 enforces
  CER ≤ 0.6 vs hypotheses; the refiner enforces nothing) — a hallucination can overwrite good ASR
  on the fine-tuned path (`pipeline.rs:1894–1897`).
- LOOP-0 fires on a single confirmation (`min_hits=1`, `corrections.rs`) — the flywheel can
  amplify one bad correction; it is at least default-off.

### F8 · Process and hygiene debt (MEDIUM)
- `PROGRESS_LEDGER.md` frozen at 2026-06-25 — **108 commits unlogged**, including the engine-default
  change; the charter requires per-iteration ledger updates.
- Locks held across inference: diarization embedder (`pipeline.rs:1032–1039`), denoiser
  (`:1041–1047`), Silero VAD session (`audio.rs:833–840`), aligner session (`aligner.rs:99–127`).
- Missing DB indices on `human_decision`/`verdict` (export/review filters table-scan);
  `agent_import_reports` grows unbounded.
- Cancellation not checked inside diarization/rediarize loops.

## 4. The plan — six phases, every one exits through a measured, user-observable gate

Rule for the whole plan: **a phase is done when its gate is observed/measured, not when its tests
pass.** One branch per phase, ledger entry per iteration, regression gate per fix.

### P0 · Truth & safety triage (1–2 days)
1. **Measure the 7B champion** on the same seed-fixed N=900 manifest (drive the warm `:8799`
   server; `scripts/scorecard_7b.py` mirroring `scorecard_finetuned.py`). Paste CER+CI into
   EVAL.md + ledger. **Decision by number:** if the 7B does not beat 21.00% [19.93, 22.04], the
   default flips to the embedded fine-tuned engine and the WSL path becomes opt-in — which also
   deletes the most fragile dependency from the default path.
2. **Close F2**: implement the documented bundled-client resolution (resolve
   `scripts/cortex_7b_client.py` from the app resource dir when the setting is empty) **or** make
   an empty path fail the import with an actionable message. Either way the settings comment and
   the code must agree; add the regression test.
3. **Purge public PII (F3)**: parameterize `wsl_omniasr_refine.py:196` via env/CLI; untrack or
   scrub the two `*_perfect_dataset.json`; remove the hygiene-gate exemptions so the gate holds the
   whole surface with zero exceptions. (History rewrite of the public repo = owner decision; record it.)
4. **Unstale the build (F4)**: rebuild frontend + exe now; add a ship-check assertion that
   `dist/` is newer than the newest `src/**` mtime (or make the release build always run
   `npm run build`).
5. **Ledger catch-up entry** covering 06-26 → today, then resume per-iteration logging.

**Exit gate:** EVAL.md carries a measured number for whatever engine is default; a fresh-profile
install either transcribes with the intended engine or refuses loudly (test proves it); public repo
hygiene gate passes with zero exemptions; the running exe contains HEAD.

### P1 · Reliability of the daily path (≈1 week)
1. **Preflight + one-click recovery**: cheap `/health` ping on `:8799` before import; on failure a
   dialog with "Start 7B server" (spawns the launcher) + auto-retry — the 5-minute worst case
   becomes 2 seconds.
2. **Rust→server direct HTTP**: replace per-segment client spawn with a direct bounded POST of the
   in-memory PCM; delete the DB-copy hack; keep the python client only as a manual tool.
3. **Import journal / resume**: per-segment pipeline_state (imported→transcribed→juried→aligned);
   on relaunch offer "Resume import" running only missing stages; kill -9 mid-import loses nothing.
4. **Lock hygiene**: move diarization/denoise/VAD/aligner inference off shared session Mutexes
   (per-worker sessions or a dedicated inference thread + channel); add cancel checks to
   diarization/rediarize.
5. DB: add `idx_segments_human_decision`, `idx_segments_verdict`; prune `agent_import_reports`
   (keep last 200).

**Exit gate (user-observable):** with the server down, import fails in <5 s with a one-click fix;
kill the app mid-100-segment-import, relaunch, resume completes without retranscribing finished
segments; UI stays responsive (get_segments <100 ms) during a full import with diarization on.

### P2 · Review speed ×3 (1–2 weeks) — beat Descript on this niche
1. **Word-click-to-edit**: click a chip → inline edit (Tab/Shift+Tab hop words, Enter commits,
   Esc cancels); modifier-click keeps play-from-word. The chip UI and word timings already exist.
2. **Single-key flow in ReviewMode**: A accept / X reject / E edit / Space play-pause / ←→ seek /
   N-P navigate — parity with ReviewInbox, fully keyboard-first.
3. **Alignment at import**: run the existing CTC forced alignment (`align_via_finetuned_mms`,
   already bounded to ≤15 s clips — every chunk qualifies by `max_segment_duration_ms=15000`)
   as a background stage after transcript persist; "Aligning words…" never blocks a reviewer again.
4. **Per-segment undo/redo** stack wired to the existing history subsystem.
5. **Suspect-first review**: hotkey jumps to next low-confidence word / next lowest-IRT segment;
   review queue ordered by expected-error × duration (scores already computed).
6. **Import progress**: per-file progress + ETA + per-file failure list, streamed via events.
7. **Instrument review throughput** (local-only): seconds-per-decision, decisions/hour — the
   honest baseline for the ×3 claim.

**Exit gate (measured):** reviewing the same 100-segment sample is ≥3× faster than the P2-start
baseline, keyboard-only, measured by the built-in instrumentation on the owner's real audio.

### P3 · Smartness that provably saves time (2–4 weeks)
1. **Measure the jury against the human record** (hundreds of `human_decision` rows already in the
   DB): T0/T1 auto-accept precision, escalation rate, per-tier confusion — surfaced on an in-app
   card. Nothing else in this phase proceeds until this number exists.
2. **Calibrate T1** threshold on that data; persist EM-fitted IRT abilities and warm-start
   (`irt.rs:182` → DB), so the consensus learns the models' real strengths over time.
3. **Diff-guard every LLM write** (`refine_text` gets the same CER≤0.6-vs-hypotheses bound as T2)
   + retry-on-timeout.
4. **LOOP-0 shadow mode**: log would-have-fired corrections without applying; show them as a
   review list; require hit_count ≥ 2 and measured over-trigger = 0 on shadow data before enabling
   auto-fire (readiness-plan bar #4: "fix a word once → app stops repeating it — measured").
5. **Close the fine-tune loop** (mostly built): one-click "export corrections as training pack" →
   owner's WSL GPU fine-tune → `import_model_checkpoint` → promotion gate (WER∧CER) → champion.
   The registry, promotion gate, and export all exist; wire the last mile and document the runbook.

**Exit gate (measured):** auto-accept precision ≥99% on ≥500 human-decided segments at a published
escalation rate; LOOP-0 shadow log shows 0 over-triggers before firing is enabled; one full
correction→retrain→promote cycle executed and the new champion measurably beats the old on the
gold set.

### P4 · Accuracy: the next measured leap (parallel, GPU-bound)
1. Publish **fine-tuned WER** (only CER exists) and run **FLEURS ckb_iq + Common Voice ckb test**
   so Cortex numbers are comparable to the literature (CV ckb open bar: 36.8 WER / 7.8 CER) —
   an honest SOTA claim or an honest gap, either is shippable.
2. **GPU for the embedded engine**: ort DirectML execution provider for MMS-1B int8 on Windows —
   measure RTF before/after (current published RTF is CPU 300M 0.0956; the 1B is the slow one).
3. **Sorani punctuation/readability pass** (research spike): no off-the-shelf ckb punctuation
   model exists; candidates: token-classifier fine-tune on AsoSoft text vs. constrained local-LLM
   restoration. Adopt only on measured human preference + no CER regression.
4. Evaluate newer open checkpoints (SeamlessM4T-v2, w2v-BERT 2.0 fine-tune, MMS-Zeroshot) on the
   N=900 gold set — adopt by measurement only.
5. Per-dialect fairness slice once CORDI access lands (charter M-fairness).

**Exit gate:** every engine selectable in-app has a same-manifest measured CER+WER row; the default
is the measured best; standard-benchmark numbers published with pinned SHAs.

### P5 · World-#1 feel (ongoing polish)
1. First-run experience: models-present / server-up / sample-clip checklist with fixes inline.
2. Session restore completes (reopen exactly where you left: segment + scroll + filters).
3. 10k-segment stress fixture in e2e; startup-time budget in CI.
4. Keep the axe WCAG gate green as panels are added; reduced-motion + focus-visible pass.
5. Docs: one-page daily-flow QUICKSTART; in-app ? overlay already exists — link it to the new keys.

## 5. What "true 10/10" means here (all observable/measured, per the readiness plan)

1. Import → good draft from a **measured-best default engine**, never silently downgraded.
2. A down dependency costs seconds (preflight + one-click fix), a crash costs nothing (resume).
3. Reviewing is keyboard-only, word-precise, ≥3× the 2026-07-02 baseline, measured in-app.
4. The jury's auto-accepts are ≥99% precise (measured), and corrections provably stop repeating.
5. Every number in the UI/docs traces to a pasted command + SHA; ledger current at every commit.
6. Zero owner PII anywhere in the public surface, zero hygiene exemptions.
7. `make ship-check` green on a clean checkout **and** the running exe is provably HEAD.

— Compiled from a full-tree read + four subsystem audits + today's gate runs. Findings F1–F8 are
the work queue; P0 is ready to start.
