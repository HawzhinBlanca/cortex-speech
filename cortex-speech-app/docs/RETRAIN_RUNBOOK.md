# RETRAIN_RUNBOOK.md — the M5 fine-tune → gate → promote cycle

This is the owner-facing runbook for turning your accumulated **human-verified** segments into a
new champion model. Every step below maps to tooling that **already exists in this repo** (an IPC
command, a script, or a library function) — nothing here is aspirational. Where a step is a script
you run in WSL rather than a button in the app, that is called out explicitly.

The one law still holds: **the promotion gate decides on a real measured CER/WER from a real
scorecard run.** No estimated numbers. If the challenger does not actually beat the champion on the
frozen gold set, it does not get promoted — that is the whole point of the gate.

**Prerequisites:** a WSL environment with the 4090 (the training + ONNX export happen there), the
fine-tuned MMS-CTC-1B HF export as the training base, and enough verified segments to matter (M3 —
keep reviewing in-app until you have a few hours of verified audio).

---

## The cycle at a glance

```
[in-app review]  ──▶  export_finetune_pack   ──▶  QLoRA train (WSL/4090)  ──▶  export ONNX
   (M3 data)          (leak-guarded pack)          (external trainer)          + verify
                                                                                   │
                                                                                   ▼
[promote champion] ◀──  gate decision  ◀──  scorecard_finetuned.py  ◀──  frozen gold eval set
 (registry)             (measured CER/WER)     (real CER + 95% CI)        (export_gold_eval_set)
```

Two exports come out of the app and **must be kept disjoint**: the training pack (what the model
learns from) and the gold eval set (what the gate measures against). The code enforces this — see
the leak guard in step 1 — but you should also never hand-mix them.

---

## Step 1 — Export the fine-tune training pack (in-app)

Run the **`export_finetune_pack`** IPC command (`src-tauri/src/commands.rs`, backed by
`eval::export_finetune_pack`). It walks every **human-verified** segment and writes, under your
chosen output dir:

- `finetune_manifest.jsonl` — one row per clip: `{audio_path, sentence, duration_seconds}` (the
  trainer's schema). `sentence` is the human gold text (corrected ▸ annotated ▸ normalized ▸ raw).
- `clips/*.wav` — a 16 kHz mono WAV per row.

It returns a `FinetunePackResult`: `total_verified`, `excluded_holdout`, `emitted`, `skipped`.

**The leak guard (do not defeat this):** every verified segment whose audio is a **holdout gold
clip** is dropped from the pack — matched by both path AND content hash via
`export::exclude_holdout_segments`. This is what keeps the training data disjoint from the eval set
the gate scores against; a non-zero `excluded_holdout` is the guard doing its job, not an error.
Rows are also deduped by (audio span, normalized text) so a re-imported duplicate is not trained on
twice, and empty-transcript / undecodable clips are skipped.

## Step 2 — Freeze the gold eval set (in-app)

Run **`export_gold_eval_set`** (→ `eval::export_gold_eval_set`). It writes `manifest.jsonl` +
`clips/` for your **gold** segments. This is the frozen benchmark the promotion gate measures on.
Freeze it once per retrain cycle and do not touch it until after the gate decision — that is what
makes the CER comparison honest and reproducible.

Convert the manifest to the TSV the scorecard scripts expect (`<clip_path>\t<reference>` per line)
if you have not already; `scripts/scorecard_finetuned.py` and `measure_finetuned_cer.py` both read a
`gold_manifest.tsv`.

## Step 3 — Train (WSL / 4090, external)

Training runs in your WSL environment, **not** in the app — there is no in-repo trainer, by design
(the app's job is to produce leak-clean data and to gate the result, not to own your training loop).
Point your QLoRA fine-tune of the MMS-CTC-1B base at `finetune_manifest.jsonl` from step 1. Keep the
run's config + base-model SHA in your notes so the resulting champion is reproducible.

## Step 4 — Export to ONNX + verify (WSL)

The app's `ort` path loads ONNX, so the trained HF checkpoint must be exported:

```bash
CORTEX_FINETUNED_MODEL=<hf_export_dir> CORTEX_FINETUNED_ONNX=<out/model.onnx> \
  python scripts/export_finetuned_onnx.py
```

Then **verify the export did not drift** — its CER must match the PyTorch model's:

```bash
python scripts/verify_onnx_export.py          # ONNX vs torch parity
python scripts/measure_finetuned_cer.py gold_manifest.tsv   # torch reference CER
```

If the ONNX CER and the torch CER disagree, stop and fix the export before gating — a broken export
would make the gate measure the wrong thing.

## Step 5 — Score the challenger on the frozen gold set (WSL)

```bash
CORTEX_FINETUNED_MODEL=<hf_dir> CORTEX_FINETUNED_ONNX=<out/model.onnx> \
  python scripts/scorecard_finetuned.py gold_manifest.tsv 3000
```

This emits the challenger's **micro CER + a seed-fixed 95% bootstrap CI** (Bisani & Ney
ratio-of-sums), using the same NFC+lower+whitespace normalization as the stock baseline scorecard,
so the number is directly comparable. **This is the real measured number the gate consumes** —
paste the exact command + gold-set SHA into `PROGRESS_LEDGER.md` per the honesty law.

Current bars to beat (real, measured): stock OmniASR-CTC-300M **29.40% CER**; the incumbent
fine-tuned MMS-CTC-1B champion **21.00% CER [19.93, 22.04], N=900**. A challenger that does not
measurably beat 21.00% on the shared gold intersection should not be promoted.

## Step 6 — The promotion gate (measured, deterministic)

The gate logic lives in `registry::decide_promotion` / `gate_and_promote` and is unit-tested. The
default `PromotionPolicy`:

- **`require_wer_beats_baseline: true`** — challenger must have lower micro-WER AND MAPSSWE
  **p < 0.05** vs the champion (the `beats_baseline` flag). This is the promotion-blocking guard.
- **`max_cer_regression_frac: 0.0`** — strict non-regression: challenger CER must be ≤ champion CER.
- **`min_cer_reduction_frac: None`** — set this to e.g. `0.30` if you want to additionally require
  the charter's ≥30% CER reduction before a promotion counts.

The gate reads the **paired** champion-vs-challenger comparison carried in the scorecard's
`vs_baseline` (computed over the shared gold-id intersection), so no separate frozen champion CER is
needed. A scorecard with no `vs_baseline` is **blocked (fail-closed)**. If no champion exists yet
for the family, the challenger is promoted as the first champion.

## Step 7 — Register + promote the new champion

Register the challenger in the model registry via the **`import_model_checkpoint`** IPC command
(`id`, `family`, `checkpoint_path`, `source`, `license`, optional `model_card_name`). Finetuned
sources (`user-finetuned` / `cortex-finetuned`) are **refused without a content hash** — the
registry will not accept an unpinned fine-tune.

**Honest gap to close before this is one-click:** `gate_and_promote` and `promote_to_champion` are
library functions with passing unit tests, but they are **not yet exposed as IPC / a Promote button**
— only `import_model_checkpoint`, `list_model_versions`, and `get_champion` are wired to the UI. So
today the final promotion is a code step (call `gate_and_promote(db, challenger_id, &scorecard,
&policy)` with the scorecard from step 5), not a button. Wiring a "Gate & promote" action that feeds
the step-5 scorecard into `gate_and_promote` is the natural next M5 code task; until then, do the
gate decision deliberately and record it in the ledger.

---

## What "done" looks like for one retrain cycle

1. `export_finetune_pack` run; `excluded_holdout` recorded (leak guard fired).
2. `export_gold_eval_set` frozen; TSV built.
3. QLoRA trained on the pack; config + base SHA noted.
4. ONNX exported and **verified** (ONNX CER == torch CER).
5. `scorecard_finetuned.py` run; **real CER + CI** pasted into `PROGRESS_LEDGER.md` with the exact
   command + gold-set SHA.
6. Gate decision computed from that scorecard against the champion — **promote only if it truly
   beats 21.00% CER under the policy.**
7. Challenger registered; champion updated (code step today); ledger entry closed.

If the challenger loses, that is a **valid, shippable result** — you keep the current champion and
you have an honest number saying the retrain did not help yet. A worse model that got promoted on a
flattering estimate is the only real failure here.
