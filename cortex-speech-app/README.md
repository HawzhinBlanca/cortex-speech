# Cortex Speech

Desktop app for Kurdish (Sorani) speech transcription, transcript curation, and dataset export.
Built with **Tauri v2**, **Svelte 5**, and Rust. The quality-first default is the separately
provisioned local **OmniASR-7B Champion** server under WSL and fails closed when it is unavailable;
smaller CTC/MMS engines are explicit offline diagnostics and are not selectable production fallbacks.

## Production features

| Feature | Details |
|---------|---------|
| **Production ASR** | WSL OmniASR-7B Champion only; unavailable means a hard stop |
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

# Fetch + SHA-256-verify required VAD/ONNX Runtime support into src-tauri/models/.
# Production ASR remains the external pinned WSL7B champion; no smaller ASR is fetched here.
npm run fetch-models

# Run in dev mode
npm run tauri dev
```

`npm run verify-models` verifies required support and any optional ASR artifacts already present.
Absent optional models are healthy; partial or hash-mismatched optional installations fail.

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

Building from source? **`npm run fetch-models`** provisions only required runtime support. OmniASR-7B
is served by the pinned WSL deployment and is never silently replaced by a bundled model.

### From source (recommended) — `npm run fetch-models`

Runs `scripts/fetch_models.py`: provisions Silero VAD and ONNX Runtime from pinned sources. Optional
diagnostics require an explicit flag, for example `npm run fetch-models -- --include-optional-asr
300m`; 1B uses `1b`, while owner-supplied MMS can only be verified with `mms`. Standard CI, release,
and verify commands pass no optional flag.

### Fine-tuned Kurdish model (explicit offline diagnostic only)

Cortex can run a **fine-tuned MMS-CTC-1B** Sorani model that roughly **halves CER** vs the stock
OmniASR (~19% vs ~42% on a matched sample — see [docs/EVAL.md](docs/EVAL.md)) and always emits Kurdish
script. It is retained for offline evaluation/diagnostics only; no production per-segment command or
review button can route dataset work through it.

To enable it, place the int8 ONNX export + vocab at **`src-tauri/models/finetuned-mms-ckb/`**:

```
models/finetuned-mms-ckb/
|-- model.onnx     # int8 Wav2Vec2-CTC export (~925 MB)
`-- vocab.json     # the MMS nested vocab (uses the "ckb" sub-map)
```

Export from a fine-tuned HF `Wav2Vec2ForCTC` with `CORTEX_FINETUNED_MODEL=<hf-dir>
CORTEX_FINETUNED_ONNX=<out.onnx> python scripts/export_finetuned_onnx.py`, then int8-quantize
(`onnxruntime.quantization.quantize_dynamic`). The file is gitignored and **not publicly fetchable**,
so it is intentionally **excluded from every standard bundle**. An isolated diagnostic build may
include it only by deliberately passing the diagnostic override:

```
npm run tauri build -- --config src-tauri/tauri.finetuned.conf.json
```

This override is never referenced by standard CI/release/verify. Without it, the installer omits MMS
and production drafting remains WSL7B-only.
Offline diagnostic code resolves the model from the app-data or bundled `models/` dir and degrades
gracefully when it is absent.

### Bundled (release installer)

The packaged Windows installer ships Silero VAD, ONNX Runtime, and the tracked WSL7B client/server
scripts. It ships no 300M/1B/MMS/Scribe ASR weights. Production requires the separately pinned,
already-running OmniASR-7B deployment.

### Automatic (in-app support models only)

The in-app model manager exposes and downloads only Silero VAD, CAM++ speaker embedding, and the
denoiser. It never exposes, fetches, or selects 300M/1B/MMS/Scribe ASR artifacts; optional ASR remains
available only through the explicit offline diagnostic instructions below.

### Optional 300M diagnostic install

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

Transcription, normalization, search, and export can run entirely on-device. No cloud API is
required. The default 7B path is not self-provisioning: configure and start the WSL Champion server
before importing. The production Settings UI does not offer a smaller-engine selector.

**Exception:** the app can download **Silero VAD v4** over HTTPS on first use. Optional CTC artifacts are installed only by explicit diagnostic tooling. Disable network access after required support is cached if you need a fully air-gapped setup.

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
