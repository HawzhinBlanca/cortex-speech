# FINAL_TEST_CHECKLIST.md — run the end-to-end final test + accept the output dataset as highest-grade

This is the turnkey procedure for the **final acceptance test**: drive the real app on real audio,
produce a dataset, and decide — against explicit, code-grounded criteria — whether the output is
**highest-grade**. Every criterion below maps to logic that already exists and is unit-tested in the
codebase (cited inline), so "pass" here means the honest, mechanical bar is met — not a vibe.

The one law still governs: **no fabricated numbers.** Every metric in the sign-off table comes from a
real run (the in-app Stats dashboard, an export artifact, or a scorecard script), pasted with its
command/source. If a number is bad, record the bad number — that is the shippable truth.

Scope: personal daily use, one user. "Highest grade" = the exported dataset is correct, leak-free,
and composed only of segments the grading rubric certifies as training-ready.

---

## Part A — produce the dataset (drive the real app)

Follow the end-to-end play in [COWORK_PIPELINE_PROMPT.md](COWORK_PIPELINE_PROMPT.md). In short:

1. **Import** a real Sorani audio folder (import → VAD chunk → ASR draft). Use the default offline
   engine; cloud LLM/STT stay **off** unless you deliberately opted in.
2. **Review + verify** every segment you intend to ship in the app: listen with the bounded-clip
   `AudioPlayer`, correct the transcript, and **Verify** (or **mark-bad** the ones that should never
   train). Human verification is what earns a segment the GOLD grade — see Part B.
3. **Validate** (`validate-btn`) and **Verify** (`verify-btn`) the dataset in-app to clear the
   structural gates.
4. **Export** from the app. For the training deliverable use **Stats → "Dataset & model tools" →
   Export fine-tune pack** (holdout-excluded) and/or the HuggingFace/JSON/JSONL/CSV/Parquet/WAV
   export. Freeze a gold eval set with **Export gold eval set** if you will score a model.

## Part B — the grade rubric (what makes a segment training-ready)

Grounded in `quality::training_grade_for_segment` (`src-tauri/src/quality.rs`). Each exported segment
carries a `training_grade` + `training_ready` (visible in the CSV/metadata columns):

| Grade | training_ready | Earned when |
|-------|:--:|-------------|
| **GOLD** | ✅ | Human-verified (verified / gold / human accept·edit) **and** no review-risk flag. |
| **SILVER** | ✅ | Jury auto/accept **and** confidence ≥ 0.85 **and** multi-agent evidence **and** no review-risk. |
| **REVIEW** | ❌ | Human-verified **but** carrying a review-risk flag (clipping / low RMS / low SNR / low-confidence or energy-heuristic alignment), or a jury-accept that misses the SILVER bar. |
| **REJECT** | ❌ | Human-rejected (mark-bad), blank, placeholder, or a **severe** audio-quality issue. |

Two correctness guarantees the rubric already enforces (do not "fix" around them):

- **Mark-bad never leaks in.** `human_rejected` is evaluated **first**, so a mark-bad clip returns
  REJECT even though it carries `verified = true` to leave the review queue.
- **Review-risk downgrades GOLD → REVIEW**, so a clipped/quiet/badly-aligned clip is never
  training-ready even if a human clicked accept.

## Part C — acceptance criteria for a "highest-grade" output dataset

Check each. All must hold for a **pass**.

1. **Only training-ready rows in the training deliverable.** The fine-tune pack
   (`export_finetune_pack`) enforces `quality::training_grade_for_segment` per row — only
   GOLD/SILVER (`training_ready`) rows ship, and refused rows (mark-bad, severe audio, placeholder)
   are counted in the result's `excludedNotTrainingReady` (shown in the export toast). *(Corrected
   2026-07-04: an earlier version of this claim was false — the pack originally admitted every
   `verified=true` row, which included mark-bad clips; the true-10 audit caught it (B1) and the
   rubric guard + regression tests now enforce it.)* Acceptance: **every row you ship for training
   is GOLD or SILVER** (spot-check `training_grade`; a non-zero refused count is the guard working).
2. **No eval-set leakage.** The fine-tune pack excludes holdout gold clips by **path AND content
   hash** (`export::exclude_holdout_segments`); a non-zero `excludedHoldout` in the result toast is
   the guard working. Acceptance: **train and gold eval sets are disjoint** (they will be, by
   construction — confirm `excludedHoldout` matches your expectation).
3. **No train→test recording leakage.** `assign_splits` keeps every segment of one source recording
   in the same split (unit-tested: `assign_splits_reproducible_and_no_recording_leakage`).
   Acceptance: **no source file appears in more than one split.**
4. **Manifest self-consistency.** For a HuggingFace export, `metadata.csv`, `dataset_infos.json`,
   the clip files, and `SHA256SUMS` must all agree (the exporter rewrites empty-split metadata and
   sorts rows by source for a stable hash). Acceptance: **SHA256SUMS verifies; every metadata row
   points at a clip that exists.**
5. **CSV integrity.** Rows are written with the `csv` crate (RFC4180 quoting) and free-text columns
   are formula-injection-guarded (`csv_safe_cell`). Acceptance: **the CSV opens with the right column
   count on every row** (Kurdish punctuation in transcripts does not shift columns) — spot-check a
   few rows with commas/quotes.
6. **Model integrity (if you trained/shipped a model).** **Stats → Dataset & model tools → Verify
   model integrity** must report checksums-match for the bundled champion.
7. **Dataset-quality signals reviewed.** In **Stats**, read the DatasetQuality panel: empty
   transcripts, low-confidence, duplicate groups, duration outliers, mean WER/CER. Acceptance: **you
   have looked at each and either cleared it or consciously accepted it** — record the numbers.

## Part D — sign-off (fill from the REAL run — no estimates)

| Item | Source | Value | Pass? |
|------|--------|-------|:--:|
| Segments verified / total | Stats dashboard | ___/___ | |
| Training-ready (GOLD+SILVER) rows shipped | export `training_grade` column | ___ | |
| REJECT/REVIEW rows in training deliverable | should be 0 | ___ | |
| `excludedHoldout` (leak guard fired) | Export fine-tune pack toast | ___ | |
| Sources spanning >1 split | should be 0 | ___ | |
| SHA256SUMS verifies | HF export | yes/no | |
| Model integrity | Verify model integrity | match/mismatch | |
| DatasetQuality: empty / low-conf / dup / outliers | Stats | _/_/_/_ | |
| (If scored) measured CER + 95% CI + N | `scripts/scorecard_finetuned.py` | ___% [__,__] N=__ | |

Paste the exact commands / screenshots behind each number into `PROGRESS_LEDGER.md`. If a measured
CER is part of the final test, the three-engine benchmark procedure is
[M1_ENGINE_DECISION_RUNBOOK.md](M1_ENGINE_DECISION_RUNBOOK.md); the retrain cycle is
[RETRAIN_RUNBOOK.md](RETRAIN_RUNBOOK.md).

---

### Reliability status backing this test (as of 2026-07-04)

The system this checklist drives is at a **certified-green** code state: full Rust suite (768 lib
tests) + clippy-all-targets + fmt + frontend (typecheck / eslint / vitest 132) + python policy gates
+ a privacy pass all green, the exe is freshness-gated to HEAD, and the dataset export + grading core
above was adversarially re-reviewed and found correct. What this checklist **cannot** self-certify is
the part that needs your hardware: the measured accuracy on real audio (the three-engine benchmark
and any retrain). Those numbers are the last inputs to an honest "highest-grade" call — run them, and
record the truth.
