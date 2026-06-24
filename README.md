# Cortex

Offline-first desktop app for **Central Kurdish (Sorani)** speech transcription, transcript
curation, and dataset export. Tauri v2 + Svelte 5 + Rust, running **Meta OmniASR CTC** locally via
**sherpa-onnx** with **Silero VAD** — no cloud required.

> The desktop application lives in **[`cortex-speech-app/`](cortex-speech-app/)**.
> Start there: [`cortex-speech-app/README.md`](cortex-speech-app/README.md) has setup, model
> placement, build, and run instructions.

## What works today (honest status)

- **End-to-end pipeline runs on real audio:** import → VAD chunk → ASR → review/annotate →
  validate → verify → export (JSON/JSONL/CSV/Parquet/HuggingFace/WAV). Fully offline.
- **Measured accuracy (not estimated):** first reproducible Sorani scorecard is **29.40% CER**
  (95% CI [26.29, 32.54], N=400, seed=42) on the stock OmniASR-CTC-300M model — see
  [`cortex-speech-app/docs/EVAL.md`](cortex-speech-app/docs/EVAL.md) for the full breakdown,
  fairness slice, and reproduction command. The model is a generalist (1600 languages), not yet
  Sorani-fine-tuned; published Sorani SOTA is ~7.8% CER, so this is an honest starting line, and
  the app is designed around **human-in-the-loop review** of the AI draft.
- **Privacy by default:** cloud LLM/STT are off unless explicitly opted in; voice is treated as
  biometric data (see [`DATA_GOVERNANCE.md`](DATA_GOVERNANCE.md)).

## Governance & process

| Document | Purpose |
|---|---|
| [`AGENT_CHARTER.md`](AGENT_CHARTER.md) | The honesty law and the bar for "done" |
| [`DATA_GOVERNANCE.md`](DATA_GOVERNANCE.md) | Dataset licenses, consent, redistribution rules |
| [`docs/`](docs/) | Roadmap, research, release process |
| [`PROGRESS_LEDGER.md`](PROGRESS_LEDGER.md) | Real, command-backed results log (no estimated numbers) |
| [`SECURITY.md`](SECURITY.md) | Security policy |

## Local quality gate

```bash
make verify-10     # repo/license/asset/provenance integrity gate (M0/M1)
make ship-check    # full local pre-release gate
```

## License

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
