# Accuracy & Hardware Plan — July 2026

**Owner request (2026-07-12):** "set a robust plan how to best do these" — the six improvement
tracks surfaced by the July-2026 tooling survey. This document is the executable plan: every track
has an **un-lieable success gate** (a number a real harness prints, never a claim), exact steps,
who does each step, and the honest risks. It follows the one law: nothing here is "done" until its
gate prints green on this rig.

**Measured rig (2026-07-11, nvidia-smi / Win32):** 2× RTX 3090 Ti 24 GB (NVLink, 4×14 GB/s),
Threadripper 3990X 64C/128T, 256 GB RAM. WSL sees both GPUs. The bf16 OmniASR-7B replica
occupies ~21.9 GB of one card.

---

## GPU budget — read this first

Both cards are spoken for the moment two workloads coexist. No dynamic scheduler (build one only
if these profiles measurably fail); pick a profile per session via env:

| Profile | GPU 0 | GPU 1 | When |
|---|---|---|---|
| `IMPORT` / `EVAL` | 7B replica | 7B replica | bulk transcription, gold evals (`CORTEX_7B_DEVICES=0,1`) |
| `REVIEW+REFINE` | 7B replica | GER corrector LLM | daily review with refinement (`CORTEX_7B_DEVICES=0`, Ollama pinned to GPU 1) |
| `TRAIN` | 7B replica | LoRA training | Track 2/4 training runs (`CORTEX_7B_DEVICES=0`) |

Hard rule: never run a 2-replica eval and the 27B-class corrector at once — it cannot fit, and a
silently CPU-spilled LLM produces garbage latency that pollutes measurements.

---

## Track 1 — Fully-local GER correction engine (highest accuracy-per-effort)

**Goal:** the existing `ger_refinement_enabled` scaffold (N-best + correction-memory prompt)
backed by a strong local LLM on GPU 1, entirely offline.

**Honest correction to the survey:** Qwen 3.6 27B at **Q8 is ~29 GB and does NOT fit one 24 GB
card**. Candidates that fit GPU 1 with KV-cache headroom: 27B-class at **Q5_K_M (~19–20 GB)**, or
a 14B-class at Q8 (~15 GB). Which one wins is an empirical question the gate below answers — not
a leaderboard question.

**Steps:**
1. **[owner or agent-with-approval] Pull candidates** via the already-installed Ollama:
   `ollama pull qwen3.6:27b-q5_K_M` and `ollama pull qwen3.6:14b-q8_0` (exact tags per the Ollama
   registry at run time). Pin Ollama to GPU 1 (`CUDA_VISIBLE_DEVICES=1` on the service) so the 7B
   replica on GPU 0 is untouched.
2. **[agent] Build `scripts/scorecard_ger.py`** — the un-lieable harness: for each frozen-gold
   clip, take the engine hypothesis (and N-best where available), run the GER prompt through the
   local endpoint, score CER/WER **before vs after** with the same normalization as
   `scorecard_7b.py`, and report the **paired seed-42 bootstrap CI of the delta**. Refuses to run
   if the endpoint is not loopback (consent gate stays structural).
3. **[agent] Shadow mode first** (project discipline, same as LOOP-0): log would-change diffs on
   real review sessions without applying. Surface over-trigger rate.
4. **[agent] A/B on frozen gold** in `REVIEW+REFINE` profile, both candidate models, plus the
   current small Ollama default as baseline.
5. **[owner decision] Promote** only the winner, only if the delta CI **excludes zero
   improvement** and added latency is acceptable (target: ≤2 s/clip on GPU 1).

**GATE (un-lieable):** `scorecard_ger.py` prints `CER <base> -> <ger>` with a 95% paired-bootstrap
CI on the delta that excludes 0, on the frozen gold manifest, command + output in the ledger.
**Risks:** GER can hallucinate "fluent" corrections that worsen CER on clean clips — the paired
CI catches this; keep the placeholder/empty guards. Prompt language matters (Sorani instructions
vs English) — A/B both.
**Effort:** harness ~half a day; model pulls are bandwidth-bound; eval ~1 h per model at 2× speed.

## Track 2 — Third jury engine: Qwen3-ASR-1.7B + ckb LoRA — **DROPPED (owner decision 2026-07-14)**

**Owner ruling: Qwen is bad in Kurdish — do not build on it.** The approved cloud ASR/judge is
strictly **Gemini 2.5 Pro** (+ ElevenLabs Scribe for cloud STT). This track is retired; a third jury
engine, if ever, must start from a model with measured ckb competence.

~~**Goal:** a fast third opinion for the jury (diversity beats solo accuracy) with native timestamp
prediction. Verified: Qwen3-ASR does **not** ship Sorani — a LoRA is required, and the 7B champion
stays champion until beaten on gold.~~

**Steps:**
1. **[agent] Data audit:** count holdout-excluded training pairs available via
   `export_finetune_pack` (verified + human-decided rows). Print hours + rows. If < ~5 h of
   verified audio, this track WAITS on the Gold Marathon (Track 6) — say so, don't force it.
2. **[agent] Fresh WSL venv** (do not touch the fairseq2 champion venv) with the Qwen3-ASR
   training stack; LoRA fine-tune on `TRAIN` profile (GPU 1). Fixed seed, frozen split, holdout
   untouched.
3. **[agent] Serve on port 8801** with the same newline-JSON line protocol (reuse the pre-fork
   server pattern; a `CORTEX_QWEN_ASR_PORT` twin), so every existing client/probe pattern applies.
4. **[agent] Score on frozen gold** with a `scorecard_qwen.py` twin. Compare against the published
   fine-tuned MMS 21.0% CER [19.93, 22.04] and the 7B champion.
5. **[Codex-coordination] Jury wiring:** third engine enters via `source_reference_models` /
   jury config — pipeline.rs is Codex territory; hand over a written spec + the working server.
6. **[agent] Jury-value measurement:** the real gate is not the solo CER — it is jury accuracy
   WITH vs WITHOUT the third engine on the same gold (T0 consensus correctness). Diversity must
   show up as a measured number.

**GATE:** solo CER on frozen gold printed by the harness AND a jury-accuracy delta with/without
the engine. Joins the jury only if the jury delta is positive with CI excluding 0.
**Risks:** 1.7B may simply lose to the fine-tuned MMS on ckb — that is a legitimate, reportable
outcome (the honest number is always shippable). Timestamp head quality on ckb is unproven.
**Effort:** venv+recipe ~1 day; training hours GPU-bound; eval ~1 h. Blocked-by-data risk is real.

## Track 3 — Precise word timestamps (kill `alignment_quality: approximate`)

**Goal:** word-level forced alignment so bounded playback, SRT/VTT export, and dataset artifacts
carry precise timings.

**Steps:**
1. **[agent] Pragmatic first pick: MMS-300M-1130 CTC aligner** (158 languages, small, offline,
   torch — runs in WSL or Windows). Build `scripts/align_words.py`: segment audio + transcript in,
   word timestamps out, emitted in the app's existing `alignment_json` shape with
   `alignment_quality: precise`.
2. **[agent] Automated sanity gate** inside the tool: monotonic non-overlapping word spans, all
   spans inside the segment window, ≥95% word coverage — refuse to emit `precise` otherwise
   (fall back to the current approximate marker; never overclaim).
3. **[agent] Batch back-fill command** for the existing library (opt-in, snapshot first) +
   per-segment alignment on future imports (ingestion side is agent-editable; if the import hook
   lands in pipeline.rs, hand Codex the spec instead).
4. **[owner, 15 min] Human spot-check:** click 20 random word-timestamps in the app and confirm
   the word you hear is the word highlighted. This is the trust gate no automation replaces.
5. **[stretch] LLM-ForcedAligner** (arXiv 2601.18220) as an A/B against MMS alignment quality if
   step 4 shows drift.

**GATE:** sanity-gate pass rate printed over the whole library (target ≥90% of segments upgraded
to `precise`), plus the owner's 20/20 spot-check logged in the ledger. SRT export flips to precise
timing only for segments that passed.
**Risks:** Arabic-script tokenization mismatches (ZWNJ, punctuation) between transcript and
aligner vocab — normalize with the app's existing normalizer before aligning.
**Effort:** ~1–2 days including back-fill run.

## Track 4 — Cheap fine-tune tricks (task arithmetic) + public benchmark

**Steps:**
1. **[agent, one afternoon] Task-arithmetic experiment** (arXiv 2601.07038): merge
   Persian/Arabic support-language task vectors into the fine-tuned MMS ckb model at 2–3 scaling
   factors; score each on frozen gold with the existing scorecard. Pure experiment — no product
   wiring unless it wins.
2. **[owner-gated, already tracked] AsoSoft-600 licensing** — once cleared, adopt the public
   ~13% WER transformer bar (arXiv 2406.02561) as an external comparison leg in `verify_10`.

**GATE:** scorecard CER per merge factor vs baseline, ledger'd. Adopt only on CI-excluding-zero
improvement; otherwise record the negative result (it is still knowledge).
**Risks:** task vectors need compatible checkpoints (same base/arch) — if the public Persian/
Arabic fine-tunes don't share the MMS base, the experiment dies early and cheaply. Say so.

## Track 5 — Chunk-parallel dispatch (the app half of the 2× import)

Server half is DONE and measured (pre-fork replica per GPU, 2.10× at concurrency 2, identical
transcripts). The app still sends chunks serially — `pipeline.rs`, **Codex territory**.

**Coordination spec (hand to Codex verbatim):**
- Bound in-flight 7B chunk requests at `min(replica_count, CORTEX_7B_CLIENT_CONCURRENCY)`,
  default 2; env-overridable, 1 = exact current behavior.
- Preserve chunk ORDER in persistence (results may arrive out of order; write-in-order or sort).
- Per-chunk error isolation unchanged (one failed chunk keeps its placeholder; others land).
- Preflight/fail-hard semantics unchanged; no change to the engine-unresolved gate.
- Acceptance: real multi-chunk import wall-time A/B (≥6 chunks) ≈2× at concurrency 2 with
  byte-identical transcripts vs serial, `npm run test:heartbeat` still green during the run.

**GATE:** the import A/B numbers from a real long file, in the ledger, plus heartbeat green.

## Track 6 — The data engine (no tool substitutes)

The calibration/trust math is already modern (conformal + IRT); it is data-starved. Plan:
- **Cadence:** ~25 real review decisions/day in Review & Correct ≈ **500 decisions in 3 weeks**
  (the Gold Marathon gate). The 2× eval/import speed and the review-surface fixes shipped this
  week exist to make this cheap.
- **At 300 decisions:** freeze the human-gold calibration split; produce reliability diagram,
  Brier/ECE, selective-risk curve (harness exists in the plan docs; agent builds the missing
  plotting script when the data exists — building it before data would be dead code).
- **Auto-accept stays shadow-only** until the error-rate upper confidence bound meets the
  ratified threshold — unchanged, non-negotiable.

**GATE:** decision count printed from the DB (`get_jobs`-style query), and at 500 the calibration
artifacts with real numbers in `docs/MEASUREMENTS.md`.

---

## Sequencing (dependency-honest)

```
Now        : commit dual-GPU work (done pending review) + this plan
Days 1–2   : Track 1 (GER harness + model pulls + gold A/B)          [REVIEW+REFINE profile]
Days 2–4   : Track 3 (aligner tool + sanity gate + back-fill)        [CPU/GPU1-light, parallel OK]
Afternoon  : Track 4 experiment (any idle GPU window)                [TRAIN profile]
Days 4–8   : Track 2 (data audit first — may wait on Track 6 data)   [TRAIN profile]
Continuous : Track 6 cadence (owner) + Track 5 Codex coordination
```

Tracks 1 and 3 are independent and can interleave. Track 2 starts with the data audit — if the
audit says "not enough verified hours," it queues behind Track 6 honestly instead of training on
noise. Every gate output lands in PROGRESS_LEDGER.md with the exact command, per convention.
