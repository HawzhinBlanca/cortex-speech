# Cortex Speech — Central Kurdish (Sorani) Accuracy Scorecard

> **Real, measured numbers.** Produced by running the live OmniASR-CTC engine on human-transcribed
> Kurdish audio and scoring against the verified references — no estimates, fully reproducible.

## Headline result (2026-06-24)

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

Data is no longer the blocker — the corpus exists and works. The two real levers now: (1) **language/
script locking** (make `ckb` conditioning effective, or fine-tune — this alone should collapse the WER
gap), and (2) **scale to ≥900 + IAA + a SeamlessM4T-v2 baseline** for a publishable, significance-tested
leaderboard entry. 29.4% CER is the honest starting line.

## Example transcriptions (ref → hyp)

| Reference | Hypothesis | Note |
|---|---|---|
| بێ ئەوەی هەستم پێ بکات | بێ ئەوەی هەستم پێ بکات | exact |
| هیچ لە دڵم دەرنەچوو | هیچ له دڵم دەرنەچوو | near-exact (orthographic) |
| گوڵەکانی وەرگرتوو ڕۆیشت | گولهكاني ورگرتو رويشت | Arabic-script confusables + drops |
| ئاخێکی هەڵکێشا | axékihalkesa | **Latin romanization failure** |
