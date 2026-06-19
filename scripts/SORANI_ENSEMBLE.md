# Sorani Diverse-Ensemble ASR

`sorani_ensemble_asr.py` runs three **architecturally-distinct** Sorani (ckb)
engines on the same audio and fuses them. Because the architectures fail
differently, their **agreement is a real confidence signal** — not a hard-coded
0.90/0.95 constant.

| Engine | Architecture | Source |
|---|---|---|
| OmniASR-LLM-7B-v2 | encoder + LLM decoder (fairseq2) | Meta omnilingual-asr (WSL fairseq2 cache) |
| Whisper-medium-ckb | seq2seq encoder/decoder | `roseman/whisper-medium-ckb` (HF) |
| Central-Kurdish-XLSR | wav2vec2 CTC | `Akashpb13/Central_kurdish_xlsr` (HF) |

Per segment it reports every engine's transcript, a **mean pairwise
character-agreement** score (0–1), and a consensus. Segments below the agreement
threshold are flagged `REVIEW`. The three transcripts are exactly the diverse
hypotheses the app's IRT confusion-network consensus (`quality/irt.rs`) is built
to fuse.

## Why diversity matters — validated on a real 36 s Kurdish clip (Nawras)

| Segment | Agreement | Note |
|---|---|---|
| 1 (0–15 s) | **0.89** | All three close. |
| 2 (15–30 s) | **0.72** | Correctly flagged as the shakiest — XLSR drifted. |
| 3 (30–36 s) | **0.84** | |

**The decisive example:** on segment 1 the 7B *dropped the year* "٢٠٢١" (its own
run-to-run non-determinism), but **Whisper-ckb and XLSR both recovered it**,
correctly verbalized as `دووهەزار و بیستوویەک`. No single model is reliable
alone; the ensemble recovers what any one misses, and the agreement score tells
you *which* segments to trust.

## Run

```bash
# In the WSL env that has omnilingual-asr (fairseq2) + transformers 4.x + hub ~0.32:
HF_HOME=/path/to/hf-cache HF_HUB_OFFLINE=1 \
  python3 scripts/sorani_ensemble_asr.py "<audio.wav>" [start_ms-end_ms ...]
```

Requires: `omnilingual_asr`, `transformers~=4.46`, `huggingface_hub~=0.32`
(newer transformers pulls hub 1.x, which breaks fairseq2 — keep 4.x), `soundfile`,
`ffmpeg`, and the model weights in the fairseq2 + HF caches.

## Next (fusion + integration)

- Replace the naive "7B-primary" consensus field with token-level ROVER / the
  app's IRT consensus over all three hypotheses (so the year the 7B dropped is
  filled from the agreeing voters).
- Wire as the app's primary ASR via `external_asr_script_path`, emitting the
  per-engine hypotheses + agreement confidence into `segment_hypotheses`.
- Use agreement confidence to drive the autonomy gate / escalation instead of the
  flat constant.
