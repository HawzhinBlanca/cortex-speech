# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- ASR engine: Meta OmniASR CTC 300M via sherpa-onnx (replaces Whisper Sorani ONNX)
- Settings labels: Meta OmniASR CTC 300M / 1B
- Model download: official sherpa-onnx OmniASR archive + Silero VAD
- Tests: OmniASR real-audio test; faster VAD/chunking unit tests (removed slow Silero proptest loop)

## [2.1.0] - 2026-05-30

### Added
- Speaker diarization (acoustic clustering) + rediarize IPC/UI
- WER/CER quality gates with validation categories
- Parquet + HuggingFace dataset export
- Real Tauri integration test (`CORTEX_INTEGRATION_TEST`) — import → export → validate
- Soak test for multi-minute synthetic imports
- Batch delete filtered segments UI
- Full i18n for stats dashboard, keyboard shortcuts modal, notifications
- CI: release build smoke, soak tests, nightly real-audio workflow
- Release checklist (`docs/RELEASE.md`)

### Changed
- Settings labels: Whisper Sorani (replacing stale OmniASR references in UI)
- Playwright E2E uses shared fixtures + locale-aware locators (36 tests)
- `CORTEX_REAL_AUDIO_DIR` env for portable real-audio test fixtures

### Fixed
- Serde camelCase for IPC types (segments, stats, quality, validation)
- Toast `aria-live` region always present for accessibility
- Tauri event mock wiring for pipeline-progress E2E tests

## [2.1.0-draft.1] - 2026-05-23

### Added
- Audio pipeline: `check_audio_file()` quick validation, `AudioInfo` struct
- DB reliability: WAL checkpoint, VACUUM, backup/restore IPC commands + frontend API
- Frontend error boundary wrapping around entire app
- Loading skeleton for segment list, stats dashboard empty state
- Responsive layout: panel collapse at <900px and <1200px with toggle buttons
- Keyboard navigation: J/K segment list nav, Shift+S/D toggle panels, ? shortcut modal
- Batch ops progress bar with real-time Tauri events
- Undo/redo toast notifications (Ctrl+Z / Ctrl+Shift+Z)
- Property-based tests: normalizer (3), diff engine (6) — 9 new proptests
- Component tests: Toast, SearchBar, ConfirmDialog — 19 new tests
- macOS and Linux CI runners with artifact upload
- E2E Playwright tests (navigation, i18n, accessibility smoke tests)
- Integration tests with real audio fixtures
- Fuzz targets for normalizer, diff, validation (nightly)
- Criterion benchmarks for normalizer, diff, audio decode
- PCM audio sample cache (10-entry LRU)
- Parallel waveform extraction via rayon
- GPU auto-detection (CUDA/CoreML/CPU fallback)
- Model verification helper
- Centralized app.config.ts
- GitHub Release workflow (v* tags, 3 OS matrix)

### Changed
- Backend logging: `env_logger` → `tracing` + `tracing-subscriber` with structured fields
- Svelte 5 migration: all 17 `svelte-check` errors fixed; use `$props()`, `mount()`, `{@render children()}`
- CSP security: added `base-uri`, `frame-ancestors`, `form-action` directives
- Session JSON storage: added production encryption note
- `once_cell` replaced with `std::sync::LazyLock`
- Performance spans: RAII SpanGuard in telemetry
- LCS diff: 10K word limit with OOM guard
- tsconfig.json: bundler moduleResolution, tests include
- tailwind.config.js: darkMode: "class"
- Audio VAD energy computation parallelized
- Settings load failure now logged

### Fixed
- Database schema: `created_at`/`updated_at` columns in base schema, `speech_segments` table name
- `get_setting()` bug: queried wrong table (`app_settings` instead of `settings`)
- Nested Mutex deadlock risk: `try_lock()` pattern in session save helpers
- Audio pipeline: `SampleBuffer::copy_interleaved_ref()` must be called after `new()` to fill data
- Blank window bug: used `new App({target})` instead of `mount(App, {target})`
- All 10 Svelte a11y warnings, all self-closing `<span />` issues
- All 7 orphaned components removed (BatchOps, DropZone, SegmentList, etc.)
- Toast role="alert" duplication fixed
- SearchBar svelte-check type error
- BatchTranscribe redo marked as unsupported
- Vec::new() double shadow in validation
- speaker_id clone optimization in stats
- telemetry mutex poison handling

## [2.0.0] - 2026-05-23

### Added
- Full-stack desktop application for Kurdish (Sorani) speech transcription
- Architecture: Tauri v2 + Svelte 5 + Rust backend
- Audio pipeline: symphonia decode, downmix, resample, VAD (energy + Silero)
- ASR: Whisper Sorani ONNX (local inference via ort)
- Normalizer: AsoSoft-based Sorani (Kaf/Yeh/Hamza/ZWNJ/Tatweel/digits) — 10 tests
- Forced aligner: MMS ONNX stub
- SQLite database (WAL mode) with segments CRUD, batch ops, search, settings
- Processing pipeline: async-free sync design with db_path-independent architecture
- Export: JSON, JSONL, CSV, Parquet
- Dataset analytics: histogram, speaker stats, duration, verification rate
- Audio fingerprinting: spectral energy dedup via blake3
- Transcript cache: blake3-hashed 1000-entry LRU

### Frontend
- Svelte 5 + TypeScript + TailwindCSS + Vite 5
- 3-panel layout: segment list, waveform/player, stats dashboard
- Waveform canvas: playhead, drag-scan, region selection
- Virtual list: windowing for 10K+ segments
- Search bar: debounced full-text with filter/sort chips
- Drag-and-drop file import
- Settings panel (General/ASR/Audio/Export)
- Keyboard shortcuts (15 shortcuts) with help modal
- Toast notifications with progress bars
- Confirm dialog for destructive actions

### Testing (Month 1)
- Undo/redo system: HistoryManager with 500-entry dual-stack
- Text diff engine: LCS-based word diff with statistics
- Vitest: 37 frontend tests (stores, diff, UI)
- Playwright: e2e test config with chromium
- Proptest: 10 property-based tests (normalizer idempotence, diff symmetry)
- Integration tests: 4 full-pipeline flow tests

### Dataset Curation (Month 2)
- Dataset validation: audio existence, empty transcripts, duration, speakers, annotations
- Audio segment export: WAV/FLAC with hound
- Batch operations: verify/unverify, speaker assign, normalize, delete
- Observer pattern: Tauri events, MPSC channel, multi-observer

### Performance (Month 3)
- Rayon parallel batch processing
- LRU memoization: normalizer (10K), diff (500)
- Frontend lazy-load component

### Developer Tooling (Month 4)
- Husky pre-commit hooks: lint-staged + typecheck + cargo check
- Commitlint: conventional commits
- Tracing: operation spans with duration, metadata, success/failure
- Changelog, Contributing guide
- EditorConfig, Prettier, ESLint

### Infrastructure
- CI: GitHub Actions (lint, build frontend, build+test)
- Release profile: LTO, strip, codegen-units=1
- Platform: Windows (MSVC), Linux tested via CI

### Fixed
- `audio.rs:decode_to_pcm()` — SampleBuffer never filled via `copy_interleaved_ref()` (all audio decode returned empty data)
- Diff algorithm — LCS alignment now correctly generates Replace operations
- History undo — test properly reflects command execution pattern

### Security
- [Planned: Month 6] CSP headers, input validation, path traversal protection
