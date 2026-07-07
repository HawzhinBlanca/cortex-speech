# Cortex Speech — Central Kurdish (Sorani) Accuracy Scorecard

> **Real, measured numbers.** Produced by running the live OmniASR-CTC engine on human-transcribed
> Kurdish audio and scoring against the verified references — no estimates, fully reproducible.

## OmniASR-7B Champion (the DEFAULT engine) — first real measurement (2026-07-02)

The deep audit's #1 gap was that the forced-default WSL 7B engine had **no** measured CER. Measured it
via the warm server (`scripts/scorecard_7b.py`, no second model load):

| Set | 7B micro CER | N | Notes |
|---|:--:|:--:|---|
| Committed CC-BY FLEURS `ckb` fixture (clean) | **29.33%** | 1 | single clip — indicative only |
| ~~Halwest verified 16 kHz set~~ | ~~59.45%~~ | ~~66~~ | ~~**data-artifact-inflated — see appendix**~~ |

**Honest reading — the 60% is the DATA, not the engine.** On the Halwest verified set the 7B scored
59.45%, but stock OmniASR-CTC-300M scored **61.69% on the identical 66 clips** (`gold_wer_real_omniasr`),
and that harness's content-overlap proxy flags most clips as **"drifted"** (ref text ↔ audio-clip
boundaries misaligned: the manifest splits text by character offset while audio is split by time, so
each clip's audio contains different words than its reference row). Both engines score ~60% because the
reference doesn't match the audio — the set is **unusable for an absolute CER**. By eye the 7B output is
coherent, correct Sorani. Net: the 7B is **not broken and is on-par-to-slightly-better than stock** on
identical data; a trustworthy **publishable** 7B CER still needs a boundary-aligned gold set (the clean
FLEURS is N=1; the original N=900 corpus that produced the fine-tuned 21% below is not on disk). The
default stays the 7B (owner's choice); the app now fails hard rather than silently downgrading (F2).

> **⚠️ CER-definition caveat (added 2026-07-07).** The 7B **59.45%** above came from `scorecard_7b.py`
> *before* its 2026-07-07 fix, which computed CER on **whitespace-STRIPPED** text (space-insensitive),
> whereas the stock **61.69%** came from the Rust `gold_wer_real_omniasr` harness, whose CER **KEEPS**
> interior whitespace (jiwer's definition, matching the fine-tuned 21% below). Space-stripping *deflates*
> CER — it hides word-segmentation errors — so these two numbers were computed on **different bases** and
> are **not directly comparable**; the *numeric* "slightly better than stock" read is therefore
> unreliable (the *by-eye* "coherent, correct Sorani" read is independent and stands). This **compounds**,
> not replaces, the reference-drift caveat — the set is unusable for an absolute CER regardless. A fair
> engine comparison needs BOTH re-measured on a boundary-aligned gold set with the now-fixed (space-kept)
> `scorecard_7b.py`. `scorecard_7b.py` is now guarded against re-drift by `test_scorecard_cer_consistency.py`.

## ⭐ Fine-tuned model — the accuracy cure, measured (2026-06-25)

A user-provided fine-tuned model, **`MMS-CTC-1B` (Wav2Vec2ForCTC, base `facebook/mms-1b-all`)**, is
now the app's opt-in engine. Two measurements: a **publishable N=900 scorecard** of the fine-tuned
model, and an **identical-clips A/B (N=50)** isolating the per-clip improvement vs the stock model.

### Publishable scorecard — the shipped (int8 ONNX) fine-tuned engine

| Metric | Value | 95% CI | N | Engine | Command |
|---|:--:|:--:|:--:|---|---|
| **micro CER** | **21.00%** | **[19.93%, 22.04%]** | 900 | MMS-CTC-1B int8 ONNX via onnxruntime | `scripts/scorecard_finetuned.py <manifest> 3000` |

- 3000-sample utterance bootstrap (Bisani & Ney ratio-of-sums; seed-fixed). Same NFC+lower+whitespace
  normalization as the stock baseline below. This is the number the **embedded engine** produces.
- vs stock OmniASR-CTC-300M **29.40%** (N=400): the fine-tune cuts CER by ~8.4 pts absolute / ~29%
  relative on the corpus, and always emits Kurdish script (no romanization/language-lock failures).

### Identical-clips A/B (N=50) — per-clip improvement

Measured head-to-head against the stock baseline on an **identical** seed-fixed gold sample
(N=50, seed=42, same corpus/chunk). Same normalization; CPU inference.

| Model (identical 50 clips) | micro CER | Output script |
|---|:--:|:--:|
| Stock OmniASR-CTC-300M (baseline) | **42.06%** | mixed (romanizes ~minority) |
| Stock + Kurdish-token constrained decode | 40.08% | Kurdish (forced) |
| **MMS-CTC-1B fine-tuned** | **19.77%** | **Kurdish (all clips)** |

**Fine-tuning roughly halves CER (42.06% → 19.77%, ~53% relative) on identical audio, and eliminates
the language-lock failures.** This is the real accuracy lever — measured, not estimated.

**Honest caveats:** (1) The publishable point estimate is the **N=900 21.00% [19.93, 22.04]** above;
this N=50 A/B is for the *per-clip* comparison only (its 19.77% is on an easier subset than the N=900
mean). (2) The 29.40% headline below was a **different N=400** sample; this random 50-subset is harder
for the stock model (42% here), so the **same-clips A/B is the fair per-clip comparison**. (3) Measured via a CPU
`transformers` harness (`scripts/measure_finetuned_cer.py`). The model is now **ONNX-exported**
(`scripts/export_finetuned_onnx.py`, external-data) and the export is **verified** — run via
onnxruntime it scores **18.57% CER** on the same 50 clips (`scripts/verify_onnx_export.py`), matching
the transformers 19.77% within export-fidelity noise. So it is loadable by the app's `ort` path; what
remains to ship it in-app is the **Rust ASR-engine wiring** (Wav2Vec2 zero-mean/unit-variance feature
normalization → ort inference → CTC decode against the model's `vocab.json`) and int8 quantization to
shrink the ~3.7 GB fp32 ONNX.
(4) The model's own `sorani_normalize.py` was not applied (kept normalization identical across models).

## Headline result — STOCK model baseline (2026-06-24)

| Metric | Value | 95% CI | N | Model | Conditioning |
|---|:--:|:--:|:--:|---|---|
| **micro CER** (primary) | **29.40%** | **[26.29%, 32.54%]** | 400 | OmniASR-CTC-300M (sherpa-onnx, int8) | stock, `language="ckb"`, no fine-tune, no LM |
| **micro WER** (secondary) | **67.62%** | — | 400 | same | same |

- CI = 3000-sample utterance bootstrap (Bisani & Ney ratio-of-sums; seed-fixed).
- Reference + hypothesis normalized identically via `SoraniNormalizer` + Unicode NFC (the path the
  metric code cross-validates against jiwer 4.0.0). Aggregation is **micro** (Σ edit-distance / Σ ref-length).

## Fairness slice (per-group micro CER)

| Group | N | CER |
|---|:--:|:--:|
| **Gender — male** | 375 | 29.66% |
| **Gender — female** | 25 | 25.37% |
| → gender disparity (max−min) | | **4.29 pts** |
| Age — twenties | 307 | 29.35% |
| Age — thirties | 51 | 24.46% |
| Age — teens | 25 | 31.72% |
| Age — forties | 11 | 33.18% |

This is, as far as we know, the **first per-gender / per-age Sorani CER breakdown**. Read the
small-N cells (female, teens, forties) as directional, not conclusive.

## ⭐ Key finding: most of the error is *script*, not recognition (N=200)

Splitting a separate N=200 run by the **script the model actually emitted**:

| Output script | Share of clips | micro CER |
|---|:--:|:--:|
| **Kurdish (Arabic block)** | **78%** | **19.71%** |
| Latin (romanized) | 21% | 94.50% |
| empty | <1% | 100% |

When the model writes Kurdish script — the large majority of clips — CER is **~20%**, far better than
the ~30% aggregate. Nearly all of the remaining error comes from the **21% of clips where it does NOT
stay in Kurdish.**

**What those clips actually are (checked by inspecting the hypotheses — correcting an earlier guess):**
a *few* are clean romanizations of the correct content (`axékihalkesa` ≈ `ئاخێکی هەڵکێشا`,
`sêrî minîandekişt` ≈ correct), but **most are garbled multilingual noise.** When the model loses
Kurdish lock it draws tokens from its 1600-language vocabulary and emits stray Chinese / Vietnamese /
etc. characters — e.g. `چەند کەس شەهید بوون` → `tsany kazy sahily kono`, `خوات لەگەڵ بێت` →
`qan la gần biệt`, `کەی بوو` → `t不`. These are genuine **language-lock failures**, *not* recoverable
by a Latin→Sorani transliterator.

**Implication — the real lever:** the model's honest Kurdish-script CER is **~20%**, and the headline
gap is dominated by **losing language lock on ~21% of clips**. The path is **model-level language
conditioning / fine-tuning** so the decoder stays in Kurdish — then scale to ≥900 + IAA + a
SeamlessM4T-v2 baseline.

## Tried in-sandbox (no retrain): constraining the decoder to Kurdish tokens

The model takes raw audio and emits CTC logits over a **9812-token, 1600-language vocabulary with only
155 Arabic-script tokens** — so per frame it can pick a non-Kurdish token. Running the ONNX directly
(`scripts/constrained_decode_probe.py`), finding the CTC blank empirically, and greedy-decoding
**unconstrained** vs **masked to {blank, space, the 155 Arabic-script tokens}**:

| Decode | micro CER (N=30 probe) |
|---|:--:|
| Unconstrained (matches the sherpa path) | 22.71% |
| **Constrained to Kurdish tokens** | **21.22%** |

Constraining **guarantees Kurdish-script output** (no more `t不` / `ite bent` in transcripts — a real
UX win) for **~+1.5 CER pts** with **no retraining**. But it is a **mitigation, not a cure**: on the
hard clips the model genuinely *mishears* (its Kurdish-token logits are weak there), so masking only
changes the script of an already-wrong output. The real accuracy lever remains fine-tuning.

**Ported to Rust (tested):** the masked greedy decode + an `ort` inference entry point now live in
`src-tauri/src/constrained_decode.rs` — 5 unit tests (keep-set, masking remap, repeat/blank collapse,
token parsing) plus a Windows parity test (`constrained_decode_real_clip_is_kurdish_script`,
`CORTEX_CONSTRAINED_WAV=…`) that loads `model.int8.onnx` via `ort` and asserts Kurdish-script output.
It is **not yet wired into the default sherpa-onnx path** — production use is an opt-in settings flag +
a cached `ort` session (small, low-risk follow-up).

## Dataset

- **Source:** "A Comprehensive Central Kurdish Sound Dataset for Robust Speech-to-Text Transformation"
  (user-provided local corpus; ~1.74M paired clips with `path, sentence, gender, age`).
- **Gold sample:** 400 clips **randomly sampled (seed=42)** from `chunk_7.zip`, restricted to real
  audio (incompressible entries — the archive contains zero-filled placeholders that were excluded),
  matched to verified Sorani transcripts + metadata, transcoded to 16 kHz mono WAV via ffmpeg.
- **License / redistribution:** **eval-only, not redistributed.** Only this aggregate scorecard is
  committed — no audio and no reference text in the repo. Confirm the corpus license before any
  train/redistribute use (`DATA_GOVERNANCE.md`).

## How to reproduce

```
# 1. Build a 4-field gold manifest from the corpus:  <wav_path>\t<reference>\t<gender>\t<age>
# 2. Run the live ASR + emit per-clip results:
CORTEX_GOLD_MANIFEST=<manifest.tsv> CORTEX_GOLD_RESULTS=<results.tsv> \
  cargo test --manifest-path src-tauri/Cargo.toml --test real_audio \
  ckb_scorecard_on_gold -- --ignored --nocapture
# 3. CER/WER + bootstrap CI + per-group fairness:
python scripts/scorecard_stats.py <results.tsv> 3000
```

## Performance — real-time factor (RTF), first measurement (2026-06-25)

| Engine | Audio | Per-inference | **RTF** | Conditions |
|---|---|---|---|---|
| OmniASR-CTC-300M (int8) | 8.22 s (FLEURS `ckb_iq` fixture) | 785.8 ms | **0.0956** | CPU, single-stream, 5 iters after warmup |

RTF = inference wall-clock ÷ audio duration; **0.0956 means ~10× faster than real-time** (model load
excluded via a warmup pass). This is a real measurement from the committed CC-BY fixture, reproducible:

```
cargo test --manifest-path src-tauri/Cargo.toml --test real_audio -- \
  --ignored omniasr_rtf_on_committed_fleurs_ckb_fixture --nocapture
```

**Caveat:** measured on a developer Windows machine, *not* a named reference machine — so it is a data
point, not a published cross-machine benchmark (charter M4.1 wants a pinned reference rig + audio set).
The test asserts only that RTF is a finite positive measurement; it prints the number for comparison
against a latency target rather than failing on a machine-dependent threshold.

## Enforced accuracy regression gate — committed CC-BY fixture (2026-06-25)

The in-repo gate `omniasr_on_committed_fleurs_ckb_fixture` (runs in the **default** `cargo test`
whenever the OmniASR model is present; skips cleanly otherwise) now enforces a real **CER ceiling** —
not just "non-blank + Kurdish-script". On the one committed FLEURS `ckb_iq` clip (CC-BY-4.0, verified
reference in `tests/fixtures/fleurs_ckb_sample.txt`):

| Engine | Clip | Measured CER | Enforced ceiling |
|---|---|---|---|
| OmniASR-CTC-300M (int8) | committed FLEURS `ckb_iq` (N=1) | **0.244** | **< 0.40** |

CTC greedy decode is deterministic, so this CER is reproducible run-to-run for a fixed model pin; the
loose 0.40 ceiling catches gross regressions (romanization, word-salad, near-blank) while tolerating a
legitimate model-pin change. **This is a single-clip regression guard, not a published scorecard** — the
publishable numbers (N=400 stock / N=900 fine-tuned, with bootstrap CIs) are above. It converts the
prior presence/script-only check into a real, committed, in-repo accuracy gate (the charter's "CI
regression gate" item), reproducible from a fresh clone after `npm run fetch-models`.

## Honest caveats (do not over-read this)

- **N=400 from one archive chunk, one corpus.** A solid first scorecard, but not multi-source; the
  charter's publishable bar is ≥900 across pinned datasets.
- **Single-source references, no IAA yet.** The corpus transcripts are taken as ground truth; the
  inter-annotator-agreement / label-noise ceiling (charter M3b) is not yet measured.
- **Gender is heavily imbalanced** (375 male / 25 female), so the female CER carries wide uncertainty.
- **The CI is an *utterance* bootstrap, not blockwise-by-speaker.** The corpus has no speaker IDs here;
  if clips share speakers, the true CI is slightly wider than shown.
- **WER (67.6%) is inflated by a script failure mode.** On a minority of clips the model emits **Latin
  romanization** instead of Kurdish script (e.g. `ئاخێکی هەڵکێشا` → `axékihalkesa`), scoring ~100% on
  those. CER is the fairer primary metric. Root cause: the `ckb` hint is a **no-op** for OmniASR-CTC
  (confirmed by `ckb_language_hint_ab`), so nothing locks the output to Kurdish script.
- **Stock model** — no Sorani fine-tune / LM / LoRA. Published Sorani SOTA is ~7.8% CER (Common Voice) /
  ~11.8% WER (AsoSoft); the gap is expected for an unadapted generalist 1600-language CTC model.

## What this tells us about the roadmap

Data is no longer the blocker — the corpus exists and works. The script split quantifies the picture:
the model's real Kurdish-script CER is ~20%, and the aggregate is dragged to ~30% by **losing language
lock on ~21% of clips** (multilingual garbage, not recoverable in post-processing). The levers, in order:
(1) **model-level language conditioning / fine-tuning** so the decoder stays in Kurdish (collapses the
language-lock losses — the biggest single win, and it needs training compute, not sandbox work);
(2) scale to ≥900 + IAA + a SeamlessM4T-v2 baseline for a publishable, significance-tested leaderboard
entry; (3) fine-tune further to close the remaining ~20% → ~8% gap to SOTA. 29.4% CER is the honest
starting line.

## Example transcriptions (ref → hyp)

| Reference | Hypothesis | Note |
|---|---|---|
| بێ ئەوەی هەستم پێ بکات | بێ ئەوەی هەستم پێ بکات | exact |
| هیچ لە دڵم دەرنەچوو | هیچ له دڵم دەرنەچوو | near-exact (orthographic) |
| گوڵەکانی وەرگرتوو ڕۆیشت | گولهكاني ورگرتو رويشت | Arabic-script confusables + drops |
| ئاخێکی هەڵکێشا | axékihalkesa | **Latin romanization failure** |

---

## Appendix: Retired measurements

### Halwest verified 16 kHz set (N=66, retired 2026-07-02)

**Measurement:** 7B scored 59.45% CER on 66 clips from the Halwest curated set; stock OmniASR-CTC-300M scored 61.69% on the same clips.

**Reason for retirement:** The 66-clip set exhibits **boundary-drift** (ref text ↔ audio boundaries misaligned). The manifest was built by splitting source text at character offsets, while the audio clips were split at time boundaries, so most clips' audio contains **different words** than their reference rows. The resulting high error rates (~60% for both engines) measure the boundary mismatch, not the engines' actual accuracy. This is a **data artifact, not a signal about model quality**.

**Honest reading:** Both engines score ~60% on this set because the references are wrong. By inspection, the 7B output is coherent, correct Sorani — on-par with or slightly better than stock on the identical audio. The set is **unusable for trustworthy CER measurement**.

**Implication:** The default 7B engine remains unmeasured on a boundary-aligned gold set. The app now fails hard on unresolvable WSL 7B (F2) and the plan calls for a real gold measurement via M1–M3 (FLEURS N=1 as a placeholder, then app-gold N≥300 from the owner's verified corrections).

This row is kept in the appendix (not deleted) as a warning: high CER numbers may reflect data issues, not model failure. Always audit your gold set for boundary integrity before trusting an accuracy claim.
