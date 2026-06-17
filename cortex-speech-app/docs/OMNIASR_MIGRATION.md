# OmniASR (Meta Omnilingual ASR) Migration Design

**Status:** Implemented (2026-05-30). Runtime uses **Meta OmniASR CTC 300M** via **sherpa-onnx**; Whisper Sorani path removed from `asr.rs`.

---

## Executive summary

| Question | Answer |
|----------|--------|
| Is OmniASR already partially wired? | **No.** Only **settings/UI ghosts** remain from an earlier design. |
| What actually runs today? | Custom **Whisper Sorani** encoder/decoder ONNX in `asr.rs` via **`ort`**. |
| Do `ctc-300m` / `ctc-1b` do anything? | **No.** They persist to `AppSettings.asr_model_size` but **pipeline ignores them**. |
| Is `sherpa-onnx` in `Cargo.toml`? | **No.** |
| What is “OmniASR” here? | **Meta `omniASR_CTC_*_v2`** (1600+ languages), served as **sherpa-onnx ONNX packs** — not a separate Meta Rust crate. |

**Effort (rough):** 2–4 engineer-weeks for production-quality swap (native deps, downloads, Kurdish QA, tests, cache migration).  
**Not trivial** — do not expect a same-day drop-in.

---

## Current state (verified)

### Backend (`src-tauri/src/`)

| Module | Role |
|--------|------|
| `asr.rs` (~1k LOC) | `KurdishAsrService` + `AsrPool`: Whisper mel → encoder → autoregressive decoder (+ optional KV cache). CUDA EP via `ort`. |
| `models.rs` | `MODELS[]` lists only `whisper_sorani_*` + Silero VAD. `resolve_models_dir()` keys off `whisper_sorani_meta.json`. |
| `pipeline.rs` | Hard-coded cache key `model_id = "whisper_sorani"`. Calls `asr_pool.with_service()` only. |
| `settings.rs` | `AsrProvider::{SherpaOnnxCtc, SherpaOnnxWhisper}`, `AsrModelSize::{CTC300M, CTC1B}` — **never read** by ASR code. |
| `features.rs` | `FbankExtractor::new_for_whisper()` — Whisper-specific mel norm. |

### Frontend

| File | Behavior |
|------|----------|
| `SettingsPanel.svelte` | Dropdown values `ctc-300m` / `ctc-1b`, labels **“Whisper Sorani”** (CHANGELOG 2.1.0 renamed labels from stale OmniASR text). |
| `settingsAdapter.ts` | Maps UI ↔ `CTC300M` / `CTC1B` only; **`asr_provider` not exposed in UI**. |
| `settingsStore.ts` | Type `AsrModel = 'ctc-300m' \| 'ctc-1b'`. |

### Dependencies (`Cargo.toml`)

- **Present:** `ort`, `ndarray`, `rustfft` (custom Whisper path).
- **Absent:** `sherpa-onnx`, `sherpa-onnx-sys`, feature flags for ASR backend.

### Docs / history

- `CHANGELOG.md` (2.1.0): UI labels fixed to Whisper; underlying **CTC enum names kept**.
- `ARCHITECTURE.md` / `README.md`: Document Whisper Sorani only.
- `export_full.py`: Exports `samil24/whisper-large-sorani-v2` to ONNX.

---

## What Meta OmniASR v2 CTC requires

### Model artifacts (sherpa-onnx distribution)

Pre-built archives (examples; see [sherpa omnilingual models](https://k2-fsa.github.io/sherpa/onnx/omnilingual-asr/models.html)):

| UI intent | Meta name | Typical sherpa-onnx bundle | Approx. size (int8) |
|-----------|-----------|----------------------------|---------------------|
| `ctc-300m` | `omniASR_CTC_300M_v2` | `sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-v2-int8-*` | ~235 MB |
| `ctc-1b` | `omniASR_CTC_1B_v2` | `sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-v2-int8-*` | ~688 MB |

Per bundle, expect at minimum:

- `model.onnx` or `model.int8.onnx`
- `tokens.txt`
- License file (check Meta / bundle README)

**Not required for CTC path:** separate encoder/decoder, BPE byte-level decode, Whisper special tokens, `whisper_sorani_meta.json`.

### Runtime

Recommended for this Tauri/Rust stack:

1. **`sherpa-onnx` crate** (official Rust bindings over C API) — `OfflineRecognizer` + `OfflineOmnilingualAsrCtcModelConfig` (see [C API example](https://github.com/k2-fsa/sherpa-onnx/blob/master/c-api-examples/omnilingual-asr-ctc-c-api.c)).
2. **Alternative (higher cost):** Re-implement CTC decode in pure `ort` — **not recommended** (duplicate sherpa work, brittle).

### Build / platform notes

- `sherpa-onnx-sys` links prebuilt or built native libs — plan for **Windows MSVC**, macOS, Linux CI matrix.
- GPU: sherpa `provider` string (`cpu`, `cuda`, `directml`, `coreml`) — align with existing `enable_gpu` + `detect_optimal_provider()` logic.
- `num_asr_threads` in settings → wire to sherpa `num_threads` (currently unused).

### Licensing

- Meta Omnilingual ASR models have **their own license** (review bundle + [facebookresearch omnilingual-asr](https://github.com/facebookresearch/omnilingual-asr) before shipping in a desktop app).
- Whisper Sorani checkpoint (`samil24/whisper-large-sorani-v2`) is a **separate** license story.

### Kurdish / Sorani quality risk (blocker for “replace”)

- OmniASR is **multilingual generalist**; app today uses **Sorani-fine-tuned Whisper**.
- **Must benchmark** WER/CER on `tests/fixtures` / `CORTEX_REAL_AUDIO_DIR` before removing Whisper.
- Settings `language: "ckb"` is **not passed** to ASR today; omnilingual CTC may need **language id / locale** if supported — confirm in sherpa docs for v2.

---

## Recommended architecture

```text
┌─────────────────────────────────────────────────────────┐
│  pipeline.rs / commands                                 │
│       │                                                 │
│       ▼                                                 │
│  trait AsrEngine { transcribe(f32 pcm, sr) -> String }  │
│       ├── WhisperSoraniEngine (ort)  [deprecate/remove] │
│       └── OmnilingualCtcEngine (sherpa-onnx)            │
│                                                         │
│  AsrPool { engine, loaded: (backend, size, gpu, dir) }  │
└─────────────────────────────────────────────────────────┘
         ▲                          ▲
         │                          │
   models.rs                  AppSettings
   (per-backend manifest)     asr_model_size, enable_gpu,
                              num_asr_threads
```

**Principles**

1. **Single pool, pluggable engine** — reload when `asr_model_size`, `enable_gpu`, or model dir changes (mirror current `AsrPool` reload logic).
2. **`models.rs` manifests** — add `omniasr_300m_meta.json` / directory layout; stop using `whisper_sorani_meta.json` as the only resolver marker.
3. **Cache keys** — use `omniasr_ctc_300m` / `omniasr_ctc_1b` (or include settings hash); migrate or invalidate existing `whisper_sorani` cache entries.
4. **Keep Silero VAD on `ort`** initially (unchanged) unless sherpa VAD is desired later.
5. **Feature flag (optional):** `asr-backend = ["whisper", "omnilingual"]` for gradual rollout.

---

## Five-step migration plan

### Step 1 — Model download & inventory

- Add `ModelInfo` rows (or separate `OMNIASR_MODELS`) with **HTTPS URLs** to k2-fsa / HuggingFace tarballs, SHA256, min sizes.
- Extract layout under `%APPDATA%/cortex-speech/models/omniasr-ctc-300m/` (etc.).
- Extend `ModelDownload` UI: real labels (“OmniASR CTC 300M v2”), progress for large archives.
- Document manual download in `README.md` until URLs are stable.

### Step 2 — Backend swap (sherpa-onnx)

- Add `sherpa-onnx` to `Cargo.toml`; prove `cargo build` on Windows in CI.
- New module `src-tauri/src/asr/omnilingual.rs` wrapping `OfflineRecognizer` + omnilingual CTC config.
- Implement `AsrEngine` trait; refactor `AsrPool` to hold `Box<dyn AsrEngine>` or enum.
- Map `AsrModelSize::CTC300M | CTC1B` → model paths.
- Pass `num_asr_threads` and `enable_gpu` into sherpa config.

### Step 3 — Settings & UI alignment

- Rename UI options to **OmniASR CTC 300M / 1B** (remove “Whisper Sorani” mislabel).
- Optionally expose `asr_provider` or remove dead `SherpaOnnxWhisper` enum variant.
- Update `resolve_models_dir()` to prefer omniasr marker when that backend is selected.
- Wire `settings.language` if sherpa supports Sorani/Kurdish conditioning.

### Step 4 — Tests & quality gates

- Update `real_audio.rs`, `pipeline_integration.rs`, soak/e2e skips to check for **omniasr** artifacts.
- Add integration test: short WAV → non-empty transcript (skip if models missing).
- Run WER/CER suite vs Whisper baseline; gate merge on regression budget.
- Update `ARCHITECTURE.md`, `RELEASE.md`, `app.config.ts`.

### Step 5 — Remove Whisper path

- Delete or feature-gate `asr.rs` Whisper decode loops, whisper `MODELS[]` entries, `export_full.py` from release path (keep script in repo optional).
- Remove `whisper_sorani_*` from required `check_models()` once omniasr is default.
- Bump cache version; document user re-transcription need.

---

## Minimal first slice (if starting incrementally)

**Not available today** — there is no sherpa stub to enable.

Smallest valuable increment:

1. Add `sherpa-onnx` dependency + **hello-world** offline transcribe in a `#[cfg(test)]` or dev-only command.
2. Download 300M int8 bundle in CI only for `CORTEX_INTEGRATION_TEST`.
3. Keep Whisper as default until Kurdish benchmark passes.

---

## Decision log

| Date | Decision |
|------|----------|
| 2026-05-30 | Investigation only; no backend swap in this change. |
| | Recommended path: **sherpa-onnx official Rust crate**, not custom `ort` CTC. |
| | UI `ctc-*` values align with **Meta OmniASR CTC sizes**, not Whisper — naming is legacy/mislabeled. |

---

## References

- [Sherpa ONNX — Omnilingual ASR](https://k2-fsa.github.io/sherpa/onnx/omnilingual-asr/index.html)
- [Model download list](https://k2-fsa.github.io/sherpa/onnx/omnilingual-asr/models.html)
- [sherpa-onnx omnilingual C API example](https://github.com/k2-fsa/sherpa-onnx/blob/master/c-api-examples/omnilingual-asr-ctc-c-api.c)
- [crates.io: sherpa-onnx](https://crates.io/crates/sherpa-onnx)
- App: `src-tauri/src/asr.rs`, `models.rs`, `pipeline.rs`, `settings.rs`
