# Cortex Speech

Production-grade desktop app for Kurdish (Sorani) speech transcription, transcript curation, and dataset export. Built with **Tauri v2**, **Svelte 5**, and a **Rust** backend that runs **Meta OmniASR CTC 300M** locally via **sherpa-onnx** for automatic speech recognition.

## Production features

| Feature | Details |
|---------|---------|
| **ASR pool** | Meta OmniASR CTC 300M loaded once, reused across all transcriptions |
| **Streaming decode** | 90s windows for long files - no full 2hr load into RAM |
| **Background jobs** | Import, single-file open, batch transcribe - UI stays responsive |
| **VAD chunking** | Podcasts/audiobooks -> many annotatable segments |
| **Quality metrics** | Duplicates, empty text, low confidence, duration outliers |
| **Speaker hints** | Multi-chunk files auto-tagged with filename stem |
| **Models** | User `%APPDATA%` dir with bundled dev fallback |

## Long-form audio (podcasts & audiobooks)

Files longer than the configured max segment duration (default **15 seconds**) are automatically split using **VAD-guided chunking**. Each chunk becomes its own annotatable segment with time offsets stored in `alignmentJson`. Import runs in a **background thread** so the UI stays responsive.

Tune chunking in **Settings -> Audio**: min/max segment duration and VAD threshold.

## Dataset workflow

1. **Import** folder (Ctrl+I) or single file (Ctrl+O)
2. **Review** chunks with bounded playback
3. **Annotate** with inline diff view
4. **Validate** dataset (Ctrl+Shift+V)
5. **Verify** segments (batch verify in sidebar)
6. **Export** JSON/JSONL/CSV/Parquet manifest, HuggingFace dataset, or WAV audio slices

See [docs/RELEASE.md](docs/RELEASE.md) for the production release checklist and headless test env vars.

## Supported Audio Formats

Import accepts the following extensions (decoded via symphonia):

`wav`, `mp3`, `flac`, `m4a`, `ogg`, `aac`, `opus`, `mp4`, `mov`, `webm`, `wma`

All audio is resampled to 16 kHz mono PCM for ASR and VAD.

## Prerequisites

- [Node.js](https://nodejs.org/) 22 and npm
- [Python](https://www.python.org/) 3.12
- [Rust](https://rustup.rs/) stable (2021 edition)
- Platform build tools for Tauri ([Tauri prerequisites](https://v2.tauri.app/start/prerequisites/))

## Setup

```bash
# Install frontend dependencies
npm install

# Fetch + SHA-256-verify the ONNX models into src-tauri/models/ (required to build from source,
# since the models are gitignored). Skips files already present + verified. See Model Placement.
npm run fetch-models

# Run in dev mode
npm run tauri dev
```

Verify models are present and unmodified at any time with `npm run verify-models` (offline SHA-256
check). The release installer already bundles the models, so end users do not run this.

Other useful scripts:

| Command | Description |
|---------|-------------|
| `npm run dev` | Vite frontend only |
| `npm run build` | Production frontend build |
| `npm run tauri build` | Full desktop bundle |
| `npm test` | Vitest unit tests |
| `npm run test:python-policies` | Python policy regressions for dataset helpers and Windows repo hygiene |
| `npm run test:e2e` | Playwright E2E tests |
| `npm run test:real-audio -- -RealAudioDir C:\path\to\fixtures` | Ignored Rust real-audio tests against local fixtures |
| `npm run test:real-audio:user -- -UserPodcastFile C:\path\to\file.wav` | End-to-end real-file ASR guard that fails on blank transcripts |
| `npm run lint` | ESLint (flat config) |

Rust tests: `cd src-tauri && cargo test`

Real-audio tests require a local fixture directory containing at least one supported audio file. Pass `-RealAudioDir` as shown above, or set `CORTEX_REAL_AUDIO_DIR` before running `npm run test:real-audio`.
Use `-SkipUnitTests` only with an integration mode such as `-Integration`; the runner rejects no-op invocations.
The user-podcast guard requires `-UserPodcastFile` or `CORTEX_USER_PODCAST_FILE`; the file must use a supported audio extension and should contain real speech, not synthetic tone fixtures.

## Model Placement

Building from source? The simplest path is **`npm run fetch-models`** (downloads + SHA-256-verifies
every model into `src-tauri/models/`). The options below cover the bundled, in-app, and manual paths.

### From source (recommended) — `npm run fetch-models`

Runs `scripts/fetch_models.py`: downloads the OmniASR archive, Silero VAD, and the ONNX Runtime
DLLs from their pinned upstreams, verifies each against a pinned SHA-256 (a mismatch is rejected — no
unverifiable artifact is placed), and writes them to `src-tauri/models/`. Re-run anytime; it skips
files already present + verified. `npm run verify-models` does the offline integrity check only.

### Fine-tuned Kurdish model (optional, embedded) — the **Fine-tuned** button

Cortex can run a **fine-tuned MMS-CTC-1B** Sorani model that roughly **halves CER** vs the stock
OmniASR (~19% vs ~42% on a matched sample — see [docs/EVAL.md](docs/EVAL.md)) and always emits Kurdish
script. It is an opt-in engine (the per-segment **Fine-tuned** button / `transcribe_segment_finetuned`
IPC command); the default Transcribe path is unchanged.

To enable it, place the int8 ONNX export + vocab at **`src-tauri/models/finetuned-mms-ckb/`**:

```
models/finetuned-mms-ckb/
|-- model.onnx     # int8 Wav2Vec2-CTC export (~925 MB)
`-- vocab.json     # the MMS nested vocab (uses the "ckb" sub-map)
```

Export from a fine-tuned HF `Wav2Vec2ForCTC` with `CORTEX_FINETUNED_MODEL=<hf-dir>
CORTEX_FINETUNED_ONNX=<out.onnx> python scripts/export_finetuned_onnx.py`, then int8-quantize
(`onnxruntime.quantization.quantize_dynamic`). The file is gitignored and **not publicly fetchable**,
so it is intentionally **excluded from the default bundle** — that keeps hosted CI and the hosted
release able to build from a fresh checkout with only `npm run fetch-models`. To bundle it into an
installer, build on a machine that has the file and pass the opt-in override:

```
npm run tauri build -- --config src-tauri/tauri.finetuned.conf.json
```

Without the override (or the file), the installer simply omits this one optional engine and the
**Fine-tuned** button reports the model is not installed — the stock engines keep working either way.
At runtime the app resolves the model from the app-data or bundled `models/` dir and degrades
gracefully when it is absent.

### Bundled (release installer)

The packaged Windows installer ships the OmniASR weights, Silero VAD, and the ONNX Runtime
DLLs inside the app — a user who installs the release does **not** need to download anything.

### Automatic (in-app, partial)

In the app, open **Settings -> AI Models** and click **Download All**. This fetches **Silero VAD**
(and other optional models with pinned hashes) into your app-data `models/` directory. The core
**OmniASR CTC 300M** archive is **not** auto-downloaded yet — its release archive SHA-256 is not
pinned, and the app refuses to fetch an unverifiable artifact — so install it via **Manual install**
below or use the bundled release. (Pinning the archive hash to re-enable OmniASR auto-download is
tracked as a release item.)

### Manual install

Download the archive from the [sherpa-onnx releases](https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-int8-2025-11-12.tar.bz2), extract it, and copy these files into **`src-tauri/models/omniasr-ctc-300m/`** (or `%APPDATA%/cortex-speech/models/omniasr-ctc-300m/` at runtime):

| File | Required |
|------|----------|
| `model.int8.onnx` | Yes |
| `tokens.txt` | Yes |
| `silero_vad_v4.onnx` | Yes (VAD; sibling of `omniasr-ctc-300m/`) |
| `onnxruntime.dll/onnxruntime.dll` | Yes on Windows (ONNX Runtime for `ort` load-dynamic VAD) |
| `onnxruntime.dll/onnxruntime_providers_shared.dll` | Yes on Windows |

On Windows the ONNX Runtime shared library is resolved next to the executable, or under the active
`models/` directory in a folder literally named `onnxruntime.dll/` (see `models::init_ort_dylib_path`).
Download the matching ONNX Runtime build from the
[onnxruntime releases](https://github.com/microsoft/onnxruntime/releases) (the version `ort` 2.0 rc
links against) and place both DLLs as shown below. The release installer bundles these for you.

Expected layout (Windows dev tree):

```
models/
|-- silero_vad_v4.onnx
|-- onnxruntime.dll/
|   |-- onnxruntime.dll
|   `-- onnxruntime_providers_shared.dll
`-- omniasr-ctc-300m/
    |-- model.int8.onnx
    `-- tokens.txt
```

Review the Meta Omnilingual ASR license in the upstream bundle before shipping in a desktop app.

### Windows build note

`sherpa-onnx` prebuilt libs use static CRT (`/MT`). The repo includes `src-tauri/.cargo/config.toml` with `target-feature=+crt-static`, and Silero VAD uses `ort` with the `load-dynamic` feature to avoid linker conflicts. Ship `onnxruntime.dll` next to the app binary for VAD at runtime (see [ort load-dynamic docs](https://ort.pyke.io/setup/linking#load-dynamic)).

## Offline-First Operation

Transcription, normalization, search, and export work entirely on-device once models are present. No cloud API is required.

**Exception:** the app can download **Silero VAD v4** and **Meta OmniASR CTC 300M** over HTTPS on first use (or via the model download UI). Disable network access after those files are cached if you need a fully air-gapped setup.

## Logging

The Rust backend uses `tracing` with `RUST_LOG` (see `.env.example`). Default level is `info`.

## Project Layout

```
cortex-speech-app/
|-- src/                 # Svelte 5 frontend
|-- src-tauri/           # Rust backend + models/
|   |-- src/asr.rs       # Meta OmniASR CTC 300M via sherpa-onnx
|   |-- src/pipeline.rs  # Import & transcription pipeline
|   `-- models/          # ONNX model files (gitignored large blobs)
|-- ARCHITECTURE.md      # Detailed system design
`-- eslint.config.js     # ESLint flat config
```

See [ARCHITECTURE.md](./ARCHITECTURE.md) for module breakdown, database schema, and long-audio chunking behavior.
