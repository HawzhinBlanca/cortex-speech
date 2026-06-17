# Architecture Guide

## System Design

```
┌──────────────────────────────────────────────────────┐
│                    Tauri v2 Shell                      │
│  ┌──────────────────────────────────────────────────┐ │
│  │           Frontend (Svelte 5 + TS)                │ │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────────────┐  │ │
│  │  │VirtualList│ │AudioPlayer│ │StatsDashboard    │  │ │
│  │  │(segments)│ │+ Waveform │ │(lazy loaded)     │  │ │
│  │  └──────────┘ └──────────┘ └──────────────────┘  │ │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────────────┐  │ │
│  │  │SearchBar │ │Settings  │ │Toast / Confirm   │  │ │
│  │  │          │ │Panel +   │ │Dialog / Keyboard │  │ │
│  │  │          │ │ModelDl   │ │Shortcuts         │  │ │
│  │  └──────────┘ └──────────┘ └──────────────────┘  │ │
│  │  i18n (EN/CKB locale store + 60+ strings)        │ │
│  │  Stores: segmentStore, settingsStore, uiStore,     │ │
│  │          notificationStore, historyStore            │ │
│  └──────────────────┬─────────────────────────────────┘ │
│                     │ IPC (invoke)                       │
│  ┌──────────────────▼─────────────────────────────────┐ │
│  │              Rust Backend                            │ │
│  │                                                     │ │
│  │  ┌─────────────┐  ┌──────────────┐  ┌───────────┐  │ │
│  │  │ commands.rs │  │  pipeline.rs  │  │  audio.rs  │  │ │
│  │  │ (20+ IPC)   │  │  (sync)      │  │ (decode)   │  │ │
│  │  ├─────────────┤  ├──────────────┤  ├───────────┤  │ │
│  │  │   db.rs     │  │ normalizer.rs│  │   diff    │  │ │
│  │  │  (SQLite)   │  │ (AsoSoft)   │  │ (LCS)     │  │ │
│  │  ├─────────────┤  ├──────────────┤  ├───────────┤  │ │
│  │  │  history    │  │  export_audio│  │ validation│  │ │
│  │  │ (undo/redo) │  │  (WAV/FLAC) │  │ (dataset) │  │ │
│  │  ├─────────────┤  ├──────────────┤  ├───────────┤  │ │
│  │  │   perf/     │  │  telemetry/  │  │ observer/ │  │ │
│  │  │(rayon,cache)│  │ (tracing)   │  │ (events)  │  │ │
│  │  └─────────────┘  └──────────────┘  └───────────┘  │ │
│  └─────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

## Key Design Decisions

### 1. Sync Pipeline (No Async)
Pipeline methods are synchronous because all I/O is blocking (file reads, SQLite). Using async would require `MutexGuard` across `.await` points which causes `Send` issues. Tauri commands can be sync, so there's no benefit to async here.

### 2. Pipeline Opens Own DB Connection
The pipeline takes a `db_path` string and opens its own connection. This avoids holding a `&Database` reference, enabling Mutex-free design and clean Rust ownership. Each processor gets its own connection.

### 3. Module Architecture

| Module | Purpose | Key Types |
|--------|---------|-----------|
| `commands.rs` | All 24+ IPC command handlers | One function per command |
| `pipeline.rs` | Import/process audio files | `ProcessingPipeline` |
| `db.rs` | SQLite CRUD + FTS5 search + backup/restore + WAL checkpoint + VACUUM + busy timeout | `Database`, `SpeechSegment` |
| `audio.rs` | Decode, downmix, resample, Silero VAD | `decode_to_pcm()`, `compute_waveform()` |
| `asr.rs` | Meta OmniASR CTC 300M via sherpa-onnx `OfflineRecognizer` | `KurdishAsrService`, GPU auto-detection (CUDA/DirectML/CoreML/CPU) |
| `normalizer.rs` | AsoSoft-based Sorani normalizer (std::sync::LazyLock) | `SoraniNormalizer` |
| `diff/` | LCS word-level diff engine | `compute_diff()`, `TextDiff` |
| `history/` | Undo/redo command pattern | `HistoryManager`, `Command` |
| `validation/` | Dataset integrity checks + output path validation | `validate_dataset()`, `validate_output_path()`, `ValidationReport` |
| `export_audio/` | Audio segment export | `export_audio_segments()` |
| `observer/` | Event/observer pattern | `ProgressObserver`, `MultiObserver` |
| `perf/` | LRU memoization + PCM sample cache + rayon parallel helpers | `Memoizer`, `PcmCache`, `parallel_batch()` |
| `telemetry/` | RAII SpanGuard operation tracing & stats | `Tracer`, `Span`, `SpanGuard` |
| `fingerprint.rs` | Audio dedup via spectral energy | `AudioFingerprint` |
| `cache.rs` | blake3-hashed transcript cache | `TranscriptCache` |
| `settings.rs` | Persisted app configuration | `AppSettings` |
| `stats.rs` | Dataset analytics | `compute_stats()`, `DatasetStats` |
| `export.rs` | Multi-format manifest export | `export_dataset()` |
| `models.rs` | Model inventory, SHA256 verification, Silero VAD download | `ModelManager`, `MODELS` |
| `aligner.rs` | Optional MMS forced aligner (energy-based fallback) | `ForcedAligner`, `WordTimestamp` |
| `error.rs` | Error types | `AppError`, `AudioError` |
| `session/` | Session state save/restore with atomic temp+rename | `SessionManager`, `SessionState` |
| `benches/` | Criterion benchmarks for normalizer, diff, audio decode | Bench functions |

### 4. Meta OmniASR CTC 300M

Transcription uses Meta **Omnilingual ASR v2 CTC 300M** through [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx):

| File | Role |
|------|------|
| `omniasr-ctc-300m/model.int8.onnx` | CTC acoustic model (int8) |
| `omniasr-ctc-300m/tokens.txt` | Vocabulary |
| `silero_vad_v4.onnx` | Voice-activity detection (Silero VAD v4) |

Models resolve from the user app-data `models/` directory when present, otherwise from bundled `src-tauri/models/`. The **ModelDownload** panel can fetch Silero VAD and the official OmniASR tar.bz2 archive.

Settings `asr_model_size`, `num_asr_threads`, and `enable_gpu` are passed into the sherpa recognizer config. CTC 300M and 1B are both supported and wired.

### 5. Long-Audio Handling

Two limits apply before and during inference:

1. **Pipeline decode cap** — `pipeline.rs` truncates decoded PCM to `MAX_PCM_SAMPLES` (16,000,000 samples ≈ 1,000 s at 16 kHz). Longer files log a warning and only the first portion is transcribed.
2. **ASR chunking** — `KurdishAsrService::transcribe()` splits audio longer than 30 s (`CHUNK_SAMPLES = 30 × 16,000`) into consecutive 30-second windows, transcribes each chunk independently, and joins the text with spaces. Failed chunks are skipped with a warning; successful chunks are still returned.

### 6. Database Schema

```sql
-- Segments table
CREATE TABLE speech_segments (
    id TEXT PRIMARY KEY,
    audio_path TEXT NOT NULL,
    raw_transcript TEXT NOT NULL DEFAULT '',
    normalized_transcript TEXT,
    annotated_transcript TEXT,
    alignment_json TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    speaker_id TEXT,
    verified INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Settings table (key-value)
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Full-text search virtual table (content-backed by speech_segments)
CREATE VIRTUAL TABLE segments_fts USING fts5(
    id, raw_transcript, normalized_transcript, annotated_transcript,
    content='speech_segments',
    content_rowid='rowid'
);
```

### 7. Frontend Store Architecture

```
locale            → 'en' | 'ckb' + derived t() translation function (60+ strings)
segmentStore      → segments[], filteredSegments, segmentStats
settingsStore     → settings (persisted to backend)
uiStore           → viewMode, panels, dialogs, processing state
notificationStore → stacked toast queue with progress
historyStore      → canUndo/canRedo, undo()/redo()
```

## Performance Characteristics

- **Audio decode**: symphonia (pure Rust) — ~10MB/s per core
- **ASR**: Meta OmniASR CTC 300M via sherpa-onnx — GPU (CUDA/DirectML/CoreML) when enabled, CPU fallback
- **Normalizer**: O(n) with LRU cache — ~1M chars/s cached
- **Diff**: O(n×m) LCS — 100 words in <1ms
- **DB**: SQLite WAL mode — 10K inserts/sec
- **Batch**: Rayon parallel — scales with CPU cores

## Security Model

- All IPC commands validated server-side with strict input validation
- File paths sanitized against directory traversal; capabilities scoped to `$APPDATA/**`
- CSP headers restrict inline scripts, media-src, worker-src, and upgrade-insecure-requests
- Session files use atomic writes (temp file + rename) to prevent partial writes
- Database backup/restore includes integrity checks (`PRAGMA integrity_check`)
- **Offline-first**: transcription and curation run without network access once models are present. Built-in downloads: Silero VAD and Meta OmniASR CTC 300M archive via `ModelManager`.

## Test Coverage

Run `cargo test` in `src-tauri/` and `npm test` at the project root. Coverage includes Rust unit/integration/property tests (normalizer, diff, audio, pipeline, DB) and frontend Vitest suites for components and stores. E2E smoke tests use Playwright (navigation, i18n, accessibility).
