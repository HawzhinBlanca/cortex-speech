# The plan to a true 10/10 — data flywheel included

**Written 2026-08-17, from measured state.** This supersedes the scattered remainders in
ROADMAP_TO_10 / TRUE_10_REMAINING for sequencing; it does not change any canon item. Where a step
needs the owner, it says OWNER. Where a step would touch the model lock, it is explicitly
promotion-by-owner, never automatic.

> [!WARNING] Historical snapshot
> This document predates the current model-attestation contract. Its duplication-weighted 7.03% CER
> result is historical evidence on a different normalization/sampling basis, not current champion or
> release proof. The current README/EVAL attestation is authoritative.

## Where we actually are (measured today)

| Fact | Value |
|---|---|
| Library | 14,735 clips / ~36.4 h, integrity ok |
| Human-labeled (accept/edit) | **426 clips / 64.7 min** |
| Labeled-data skew | **94.7 %** of labeled duration from ONE recording (Lamofull2) |
| Distinct labeled speakers | 8 |
| Frozen gold holdout | 348 clips (`gold_segments`) + FLEURS ckb |
| Historical champion snapshot | OmniASR-7B + owner LoRA, **7.03% CER** on the historical duplication-weighted FLEURS basis; not a current SOTA or release claim |
| Cleaned corpus staged | **2,676 files / ~550 h**, provenance-declared (v54) |
| Model registry | `adapters` / `dataset_runs` tables exist, **0 rows — not yet authoritative** |
| Sweep | 39 gates; one red: `spot-check-pool` 21/24 (OWNER: 3 adjudications) |
| Flywheel honest score | curation ~7/10, correction→better-live-model ~3/10 |

The app-reliability audit is closed (PR #61). What separates today from 10/10 is **one owner
action, one data problem, and one missing loop** — in that order of difficulty.

## The governing rule (from the 2026-08-17 flywheel audit, adopted)

> Record every decision; include every eligible **latest** human label **exactly once** in the next
> **immutable** batch; **prove** the challenger is better before promotion.

Canon constraints that shape everything below:
- **Model lock:** the champion family is fixed. The flywheel trains the owner's own LoRA on the SAME
  OmniASR-7B 300M/1B/7B family — never another family, and nothing ever replaces the live adapter
  without the owner's explicit promotion.
- **Verbatim law:** training text is human ▸ champion-raw. Machine-refined text never becomes a label.
- **FLEURS stays frozen**; the promotion gate additionally needs an OWN speaker-disjoint holdout.

---

## Phase 0 — Land what is in flight (today; blocks nothing else)

1. PR #62 (processed-audio provenance) merges on green — watcher armed. **DONE when merged.**
2. **OWNER: 3 desktop adjudications** → `spot-check-pool` 21 → 24 → every kept sweep gate green.
   This is the only red gate and no code can close it.
3. **OWNER: re-review the 12 historical wrong-dialect decisions** flagged by `reviewer-queues-live`.

**Exit gate:** `verify_10.py` fully green at HEAD.

## Phase 1 — Break the data skew (weeks 1–2; the real 10/10 lever)

The flywheel is pointless while 94.7 % of labels are one voice. The 550 h cleaned corpus fixes
this, imported in inspectable batches — never one run.

1. Import ~25–50 h per batch via `batch_importer` (champion must be up first — supervision lives in
   the app; either keep the app open or start the server before a headless run).
   Order batches for **speaker diversity**: many small books before finishing any single one.
2. After each batch, the standing gates run: `dataset-duplicates` (baseline 0), provenance
   declarations present, count gates.
3. Build reviewer queues per dialect map; reviewers work the phone links.
   Throughput is the bottleneck: at the measured pace, plan for **≥ 25 h labeled across ≥ 40
   recordings** before any retrain is worth gating.
4. **OWNER: more Sorani source** — Sorani-only reviewers have ~1.1 h runway.

**Exit gate (measured, not vibes):** labeled duration ≥ 25 h AND top-1 recording share ≤ 30 %
AND ≥ 25 distinct labeled speakers.

## Phase 2 — Immutable snapshots + a training pack that carries its provenance (week 2)

Fixes the audit's two true pack findings.

1. `export_finetune_pack` grows: `split` (reusing export.rs's leakage-safe union-find groups),
   `snapshot_id`, `decision` (accept/edit), `decision_revision`, `source_recording`, and the v54
   processed-audio flag per row.
2. **Immutable snapshot:** each pack export writes a `dataset_runs` row — content hash of the
   manifest, counts, exclusion tallies, created_at. Same hash ⇒ same snapshot; nothing is ever
   edited in place.
3. "Exactly once": rows keyed by latest decision revision; superseded edits and undone decisions
   are excluded and **counted** in the tally (silent exclusion is the recurring honesty bug).
4. Policy test: same DB ⇒ byte-identical manifest; holdout leak guard proven with a planted clip.

**Exit gate:** two consecutive pack exports of an unchanged DB hash identically; `dataset_runs`
row written; policy test in the sweep.

## Phase 3 — Wire the challenger loop (weeks 3–4; OWNER-gated to run)

Training stays external (WSL) by design — what gets built is the WIRING, so a run is one command
and its evidence is machine-checked.

1. `scripts/train_challenger.py` (WSL): reads ONE snapshot by id, trains a LoRA on the champion's
   base, writes the adapter + a run manifest (snapshot hash, base SHA, config) — reproducible.
2. Eval harness: score champion vs challenger on (a) FLEURS ckb, (b) the own speaker-disjoint
   holdout (grown from `gold_segments` as Phase-1 data arrives), (c) protected slices — per-dialect,
   per-speaker-bucket, noisy-clip bucket. `mapsswe_compare.py` supplies significance.
3. Promotion report: one JSON verdict — PROMOTE only if overall CER improves significantly AND no
   protected slice regresses beyond noise. Anything else is REJECT with numbers.
4. First run is a **canary**: today's 426-clip data, expected verdict REJECT — the point is proving
   the loop end-to-end, not winning.

**Exit gate:** canary run completes snapshot → train → eval → verdict with zero manual steps
between, and the verdict is honest (a REJECT on today's data is a PASS of the loop).

## Phase 4 — Authoritative registry, promotion, rollback (week 4+; OWNER approves each promotion)

1. `adapters` becomes the single source of truth: adapter path, snapshot hash, eval report hash,
   status (candidate/champion/retired). DB already enforces at-most-one-champion-per-family.
2. Promote = one transaction: registry flip + server adapter reload + health check (the 89 s
   self-start path already exists) + automatic rollback to the prior adapter on a failed health
   check. The champion server reads its adapter path FROM the registry, not from env.
3. Drill it like the backups were drilled: promote the canary, serve one real clip, roll back,
   prove byte-identical serving of the old champion. The drill is a sweep gate.

**Exit gate:** promotion drill green in the sweep; `CORTEX_7B_MODEL_DIR` env override retired.

## Phase 5 — Redefine done (after 1–4)

`verify_10.py` gains three gates: `snapshot-immutability`, `challenger-loop` (last canary run
present + honest), `promotion-drill`. Then "10/10" = every kept gate green **including the
flywheel**, and the README's definition of done points at this file.

---

## What this plan deliberately does NOT do

- **No auto-retraining.** Batches, owner-triggered. The canon's model lock stands; the flywheel
  produces *evidence for a promotion decision*, never a silent swap.
- **No new model families.** LoRA on the existing champion base only.
- **No per-click training** — the audit is right that it would overfit to hard failures; accepts
  balance edits, and older gold replays in every batch.
- **No 550-h bulk import in one shot.** Batches with gates between, because the one time we
  skipped inspection we shipped 170 duplicates.

## Sequencing truth

Phases 0–2 are independent of each other and can all start now. Phase 3 without Phase 1's data
produces only canaries (still worth wiring). Phase 4 without Phase 3 has nothing to promote. The
single longest pole is **human review throughput** — everything else is days of engineering.
