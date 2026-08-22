# Cortex

Desktop app for **Central Kurdish (Sorani)** speech transcription, transcript curation, and dataset
export. Tauri v2 + Svelte 5 + Rust. The quality-first default is the local **OmniASR-7B Champion**
server under WSL. Smaller 300M/1B/MMS models remain explicitly installed diagnostics; standard
release builds never bundle or select them as production fallbacks. No path requires a cloud service.

> The desktop application lives in **[`cortex-speech-app/`](cortex-speech-app/)**.
> Start there: [`cortex-speech-app/README.md`](cortex-speech-app/README.md) has setup, model
> placement, build, and run instructions.

## What works today (honest status)

- **End-to-end pipeline runs on real audio:** import → VAD chunk → ASR → review/annotate →
  validate → verify → export (JSON/JSONL/CSV/Parquet/HuggingFace/WAV). The default Champion path
  requires its separately provisioned WSL model server to be healthy before import; it fails closed
  instead of silently downgrading.
- **Historical diagnostic accuracy (not production-champion evidence):** the first reproducible
  Sorani scorecard measured **29.40% CER** (95% CI [26.29, 32.54], N=400, seed=42) on stock
  OmniASR-CTC-300M. It is retained in [`cortex-speech-app/docs/EVAL.md`](cortex-speech-app/docs/EVAL.md)
  for reproducibility, but it does **not** measure the current OmniASR-7B production champion and
  does not authorize the 300M model as a runtime fallback.
- **Privacy by default:** there is no shipped cloud-ASR/STT path. Cloud LLM and advisory Listening
  Jury audio egress are separate, explicit opt-ins; voice is treated as biometric data (see
  [`DATA_GOVERNANCE.md`](DATA_GOVERNANCE.md)).

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
make governance-proof  # repo/license/asset/provenance integrity gate (M0/M1)
make ship-check        # CI-equivalent local pre-release gate
make verify-10         # full charter gate; fails closed until eval/egress/release proofs are live
```

## License

**PolyForm Noncommercial 1.0.0** — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

The source is public for transparency and portfolio purposes. Noncommercial use (personal,
academic, research, evaluation) is freely permitted. **Commercial use — including embedding
this code, in whole or in part, into another product or service — is NOT permitted** without a
separate commercial license from the copyright holder. To request one, contact
hawzhin88@gmail.com.

Bundled third-party components (Meta OmniASR, sherpa-onnx, Silero VAD, AsoSoft) retain their
own upstream licenses — see [`NOTICE`](NOTICE) and [`THIRD_PARTY_LICENSES.md`](cortex-speech-app/THIRD_PARTY_LICENSES.md).
