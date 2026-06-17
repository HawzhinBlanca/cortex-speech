# Roadmap to 10/10 — Reality-Checked 2026-06-12

Status after adversarial audit: **7.5/10**. Confidence pipeline, Heh normalization,
punctuation unification, digit verbalization, and MAX_PCM enforcement are REAL and
tested (136 Rust + 73 frontend tests green). The items below are what remain, in
priority order. Each item lists the files to touch and a concrete acceptance test.

---

## P0 — Correctness bugs in shipped code (fix first)

### 1. Heh normalization ordering bug
`normalizer.rs:82` removes tatweel (U+0640) **before** `normalize_heh_contextual()`
runs at line 88. In Sorani orthography, word-final consonant /h/ is written `هـ`
(heh + tatweel) precisely to distinguish it from the vowel `ە`. Stripping tatweel
first destroys that signal, so words like `گوناهـ` become `گوناە` (wrong — that
final letter is /h/, not /ə/).

- **Fix:** in `normalize()`, protect `هـ` before tatweel removal: replace word-final
  `ه + U+0640` with `ھ` (U+06BE) *first*, then strip remaining tatweel, then run the
  contextual pass.
- **Accept:** unit test `گوناهـ` → `گوناھ` (or documented canonical form), existing
  Heh tests still pass.

### 2. Verify CTC confidence is non-empty at runtime
`asr.rs:370-375` reads `ys_log_probs` from the sherpa-onnx result JSON. sherpa-onnx
populates this for **transducer** models; for **CTC** models it may be empty, which
would make every confidence silently `None` while all code downstream still
"works". The unit tests cannot catch this (model-dependent tests are ignored).

- **Fix/verify:** run one real transcription with the OmniASR CTC model and assert
  `confidence.is_some()`. If empty, compute confidence from token-level
  `timestamps`+`tokens` posteriors or patch sherpa bindings.
- **Accept:** an `#[ignore]`-gated integration test that transcribes a bundled 5s
  WAV and asserts confidence ∈ (0,1]; run it manually in CI-with-models.

### 3. Compare-ASR is still a stub
`App.svelte:106-137` admits "we don't have a dedicated backend command yet" and
runs one model. Either implement a real `compare_asr` IPC command that loads 300M
and 1B sequentially and returns both texts + confidences + diff, or **remove the
button**. Half-features cost credibility.

---

## P1 — Dataset quality plumbing (gets to ~8.5)

### 4. Audio quality gates at import (SNR / clipping / silence)
No RMS, clipping, or SNR measurement exists anywhere in `src-tauri/src`.

- **Files:** new `src-tauri/src/audio_quality.rs`; call from `pipeline.rs` after
  decode; add `clipping_ratio REAL`, `rms_db REAL` columns (migration v4); surface
  in `validation/mod.rs` and ValidationPanel.
- **Accept:** importing a clipped file flags a warning; validation report counts
  low-RMS segments.

### 5. ASR-internal 30s chunking: cut on silence, not mid-word
Pipeline-level VAD chunking is real, but the fallback in `asr.rs:275-327` still
slices at hard 30s boundaries with no overlap. Any segment >30s (max segment
duration is user-configurable, so this happens) risks mid-word splits.

- **Fix:** before each hard cut, search ±1.5s around the boundary for the
  lowest-energy 100ms window and cut there; or add 1s overlap + longest-common-
  suffix/prefix merge of the two chunk texts.
- **Accept:** test with synthetic 65s audio; no word fragments at joins.

### 6. HuggingFace export: real, loadable datasets
`export.rs:56-125` still writes a single `train.jsonl` with absolute local paths —
not loadable with `load_dataset()` anywhere else.

- **Fix:** add a split assignment step (train/dev/test, ratio + seed in settings)
  with **speaker-disjoint** option (group by `speaker_id` before splitting);
  copy/transcode segment audio into `data/<split>/`; write `metadata.csv` or
  per-split jsonl with *relative* paths (HF `audiofolder` convention); README data
  card with language `ckb`, license field (settings), provenance (app version,
  model used, export date), per-split duration stats.
- **Files:** `export.rs`, `settings.rs`, `migrations` (add `split TEXT` column),
  SettingsPanel + ExportDialog.
- **Accept:** `datasets.load_dataset("audiofolder", data_dir=...)` succeeds on an
  exported folder on a clean machine.

### 7. Audit trail
Edits overwrite silently. Add `segment_edits` table (segment_id, field, old, new,
source: asr|human|normalizer, timestamp) written from `db.rs::update_segment`.
Expose "history" per segment in HistoryPanel. Required for dataset provenance.

---

## P2 — Annotation UX parity with ELAN/Prodigy (gets to ~9)

### 8. Segment boundary editing (currently FAKE)
`Waveform.svelte` shift-drag selection is never persisted; `onRegionSelect` isn't
even passed by `App.svelte:1167`. 

- **Fix:** draggable start/end handles on the waveform; on release, invoke a new
  `update_segment_bounds` command that rewrites `alignment_json` offsets,
  re-slices duration_ms, and (optionally) re-transcribes the adjusted span.
- **Accept:** drag a boundary, reload app, boundary persisted; audio playback
  respects new bounds.

### 9. Loop playback (currently absent)
Add loop toggle to `AudioPlayer.svelte` (replay segment on `ended`/endTime hit).
One afternoon of work, large review-speed payoff.

### 10. RTL (currently FAKE — zero `dir` handling)
- Bind `dir={$locale === 'ckb' ? 'rtl' : 'ltr'}` on the app root (`App.svelte:873`);
- `dir="auto"` on all transcript textareas and search input;
- audit `text-right`/padding classes under `[dir="rtl"]`;
- ship a proper Kurdish font stack (e.g. Vazirmatn/Noto Naskh Arabic) in `app.css`.
- **Accept:** ckb locale renders mirrored layout; cursor behaves in mixed
  Kurdish/Latin text.

### 11. Advanced filters
SearchBar has only verified/pending + sort. Add filter chips: confidence range,
duration range, speaker, has-annotation, WER bucket (when gold exists). All data
already in the store — UI work only.

---

## P3 — The 9.5→10 differentiators

### 12. Eval harness (gold set + WER tracking)
- Add `is_gold BOOLEAN` to segments (migration); "mark as gold" batch op.
- New `eval.rs`: run ASR over gold set, compute WER/CER per model / per speaker /
  over time; persist runs to an `eval_runs` table.
- Dashboard panel: WER trend chart, per-speaker breakdown.
- This turns the app into the de-facto Kurdish ASR benchmark tool — nobody else
  has this.

### 13. Lightweight multi-annotator support
Not full Label Studio. Just: `annotator_id` on edits (from a settings-level
display name), double-annotation sampling (assign N% of segments to a second
pass), and an agreement view (per-segment text diff + corpus-level agreement %).

### 14. WSL 7B path hardening
`pipeline.rs` hardcodes `/root/cortex_env/bin/python3`, returns no confidence, and
has no graceful "WSL not installed" error. Make path configurable, parse errors,
return confidence if the Python side provides token scores — or mark the feature
experimental in the UI.

---

## Explicitly verified as DONE (do not redo)
- End-to-end confidence: sherpa → `pipeline.rs:608` → DB migration v3 → IPC →
  sortable badge UI in App.svelte ✓ (pending P0-2 runtime check)
- Contextual Heh + Teh Marbuta mapping ✓ (pending P0-1 ordering fix)
- Arabic punctuation unification, digit verbalization option ✓
- MAX_PCM_SAMPLES enforced via streaming decode + chunk caps ✓
- FTS5, WAL, migrations, cancellation, telemetry, PCM cache ✓
- 136 Rust + 73 frontend tests green ✓
