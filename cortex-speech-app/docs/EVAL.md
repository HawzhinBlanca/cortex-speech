# Cortex Speech — Central Kurdish (Sorani) Accuracy Scorecard

> **First real, measured number.** This is produced by running the live OmniASR-CTC engine on
> human-transcribed Kurdish audio and scoring it against the verified references — no estimates.

## Headline result (2026-06-24)

| Metric | Value | N | Model | Conditioning |
|---|:--:|:--:|---|---|
| **micro CER** (primary) | **34.5%** | 40 | OmniASR-CTC-300M (sherpa-onnx, int8) | stock, `language="ckb"`, no fine-tune, no LM |
| **micro WER** (secondary) | **79.4%** | 40 | same | same |

Reference orthography normalized identically for ref and hyp via `SoraniNormalizer` + Unicode NFC
(the same path the metric tests cross-validate against jiwer 4.0.0). Aggregation is **micro**
(total edit distance / total reference length).

## Dataset

- **Source:** "A Comprehensive Central Kurdish Sound Dataset for Robust Speech-to-Text Transformation"
  (user-provided local corpus; ~1.74M paired clips with `path, sentence, gender, age`).
- **Gold sample:** 40 short clips drawn from `chunk_7.zip`, selected by **incompressibility** (real
  mp3, not the zero-filled placeholder entries present in the archive), matched to their verified
  Sorani transcripts, transcoded to 16 kHz mono WAV with ffmpeg.
- **License/redistribution:** the corpus is **eval-only / not redistributed** here. Only this
  aggregate scorecard is committed — no audio and no reference text are checked into the repo.
  (Confirm the corpus license before any train/redistribute use; see `DATA_GOVERNANCE.md`.)

## How to reproduce

```
# Build a gold manifest (TSV: <wav_path>\t<reference_sentence>) from the corpus, then:
CORTEX_GOLD_MANIFEST=<manifest.tsv> \
  cargo test --manifest-path src-tauri/Cargo.toml --test real_audio \
  ckb_scorecard_on_gold -- --ignored --nocapture
# prints per-clip ref/hyp and:  [scorecard] N=.. micro_CER=.. micro_WER=..
```

## Honest caveats (do not over-read this number)

- **N=40 is a spike, not the charter bar (≥900).** It samples one archive chunk; treat it as a
  first signal, not a publishable leaderboard entry. No bootstrap CI is attached at this N.
- **Single-source references, no IAA yet.** The corpus transcripts are taken as ground truth; no
  inter-annotator-agreement / label-noise ceiling has been measured (charter M3b). A real published
  scorecard must cite that ceiling.
- **WER (79.4%) is inflated by a script failure mode.** On a minority of clips the model emits
  **Latin romanization** instead of Kurdish script (e.g. `ئاخێکی هەڵکێشا` → `axékihalkesa`), which
  scores ~100% on those clips. CER (34.5%) is the fairer primary metric; the romanization clips are
  a language-locking problem, confirmed by the `ckb_language_hint_ab` finding that the `ckb` hint is
  a **no-op** for OmniASR-CTC.
- **This is a STOCK model** — no Sorani fine-tune, no language model, no LoRA. Published Sorani SOTA
  is ~7.8% CER (Common Voice) / ~11.8% WER (AsoSoft). The ~34.5% CER gap is the expected distance for
  a generalist 1600-language CTC model with no Kurdish adaptation.

## What this tells us about the roadmap

The headline accuracy work is no longer blocked on "is there data" — it is. The real levers now are
(1) **language/script locking** (make the `ckb` conditioning effective, or fine-tune) and (2) a
**larger gold set + IAA** before publishing a leaderboard CER. The number above is the honest
starting line.

## Example transcriptions (ref → hyp)

| Reference | Hypothesis | Note |
|---|---|---|
| بێ ئەوەی هەستم پێ بکات | بێ ئەوەی هەستم پێ بکات | exact |
| هیچ لە دڵم دەرنەچوو | هیچ له دڵم دەرنەچوو | near-exact (orthographic) |
| گوڵەکانی وەرگرتوو ڕۆیشت | گولهكاني ورگرتو رويشت | Arabic-script confusables + drops |
| ئاخێکی هەڵکێشا | axékihalkesa | **Latin romanization failure** |
