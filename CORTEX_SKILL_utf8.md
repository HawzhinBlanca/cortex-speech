# Agent Skill: Kurdish (Sorani) Speech Dataset Ingestion & Curation Pipeline

A highly optimized skill guide for agentic AIs (such as Antigravity, Codex, or custom developer subprocesses) to ingest raw audiobooks, podcasts, or interviews, execute step-by-step curation pipelines, and generate professional-grade, high-quality Sorani Kurdish speech training datasets.

---

## 🎯 Skill Objective

Transform raw audio files (of any duration) into clean, character-normalized, word-aligned, speaker-tagged, and quality-vetted speech datasets ready for training state-of-the-art TTS (e.g., VITS, Coqui) or ASR (e.g., Whisper, Wav2Vec2) models.

---

## 🤖 The Agent Curation & Verification Mandate (Accurate Transcription)

As an AI agent running this skill, **you are not merely a script runner**. You are the primary quality inspector. You must actively audit, correct, and verify the transcription text at every step:
1. **Quality Check Raw ASR:** Do not blindly trust the recognizer. Check the transcripts returned by the model. Check for missing prefixes/suffixes or incorrect phoneme splits.
2. **Model Comparison Gate:** For complex audio segments, trigger the ASR comparison utility (`compare-asr`) to evaluate OmniASR 300M versus the 1B parameter model. Manually or programmatically select the model output with the highest lexical accuracy.
3. **Orthography Verification:** Verify that Sorani characters (`ڕ`, `ڵ`, `ۆ`, `ێ`, `ە`, `ڤ`) are mapped correctly. Correct any instances where standard Kurdish spelling is mangled.
4. **Forced Alignment Feedback Loop:** If the forced aligner falls back to proportional energy splitting (`fallback_align`), manually review the transcript bounds. Adjust transcript text to remove pauses or sound artifacts that skew alignment boundaries.

---

## 🛠️ Step-by-Step Execution Protocol

### Step 1: Model Pre-Flight Verification
Before processing, ensure that ONNX neural models are cached locally to guarantee offline processing speeds.
1. Check that Silero VAD is cached at `%APPDATA%/cortex-speech/models/silero_vad_v4.onnx`.
2. Check that the OmniASR CTC 300M model folder is present under `models/omniasr-ctc-300m/` with `model.int8.onnx` and `tokens.txt`.
3. If missing, programmatically call the `download_model` Tauri command to warm up the model directory.

### Step 2: Background Ingestion & VAD-guided Chunking
Long-form audio (podcasts, audiobooks, interviews) must be chunked into short, annotatable slices (optimal length: **2.0 to 15.0 seconds**).
1. Call `import_audio_file` or `import_directory` via Tauri IPC.
2. Monitor progress via the `ProgressObserver` telemetry channel (checking `files_processed` and `pipeline_status`).
3. Ensure the backend executes:
   * **Mono downmixing and 16kHz resampling** (via `audio.rs` decoding).
   * **Silero VAD window segmentation** to cut long files on natural breath pauses.
   * **Automatic Speaker lanes tagging** (e.g., assigning files a filename-stem-based speaker ID).

### Step 3: Denoising & Automatic Speech Recognition (ASR)
Transcribe the speech chunks using Meta OmniASR.
1. Run `batch_transcribe` for the newly ingested segment IDs.
2. **GPU Driver Fallback Verification:** Ensure the engine detects GPU acceleration (DirectML/CUDA). If GPU providers fail, verify that the recognizer catches the warning and re-initializes on `"cpu"` provider without crashing.
3. Confirm that segments have populated `rawTranscript` values in the SQLite database (`cortex-speech.db`).

### Step 4: Sorani Kurdish Text Normalization
Spelling inconsistencies (e.g., mixing Arabic Yeh `ي` and Kurdish Kehe `ک` instead of Sorani `ی` and `ک`, or double spacing) corrupt dataset training.
1. Run `batch_normalize` on the segments.
2. Confirm the normalizer applies the **AsoSoft standard rules**:
   * Map Arabic Kaf `ك` to Kurdish Kehe `ک`.
   * Map Arabic Dotless Yeh `ى` and Dotted Yeh `ي` to Kurdish Dotless Yeh `ی`.
   * Normalize specific sequences (e.g., spacing surrounding Kurdish pseudo-spaces/tatweel).
3. Validate that `normalizedTranscript` contains standard Kurdish unicode characters.

### Step 5: High-Precision Word-Level Forced Alignment
Calculate word boundaries for word-sync subtitle visualizers and TTS training.
1. Trigger `align_segment` using the normalized text.
2. The aligner calculates word-boundary frame offsets using the CTC token probabilities.
3. Validate that `alignmentJson` contains valid array maps of `[{ "word": "...", "start": 0.0, "end": 0.0 }]` and merges cleanly into the database.

### Step 6: Visual Interactive Curation & Quality Gates
Review and polish the segments to achieve "10/10" precision.
1. **Interactive Timeline Scrubbing:** Review the waveform zoom timeline. Scrub to specific word timestamps using click-to-seek, and edit typos using the double-click/Enter edit field.
2. **Perform Dataset Quality Validation:** Run `validate_dataset` to run automated quality gates:
   * **Mean WER / CER calculations:** Flag segments exceeding maximum thresholds (e.g., WER > 35%).
   * **Duration Outliers:** Flag clips `< 0.5s` or `> 20.0s`.
   * **Duplicate Groups:** Flag duplicate transcripts to avoid dataset contamination.
   * **Empty Transcripts:** Catch silent VAD slices.
3. Edit annotations and confirm changes are recorded in the visual **History Stack** (allowing multi-level Undo/Redo).

### Step 7: Export Manifest & Audio Slices
Once curation is verified, export the final training assets.
1. Call `export_dataset` to generate manifests in **Parquet**, **JSONL**, **CSV**, or standard **JSON** formats.
2. Call `export_audio` to cut the master audio files into individual, verified WAV slices (resampled to 16kHz mono, named with segment UUIDs) alongside metadata manifests.
3. Alternatively, export to a standardized **Hugging Face Dataset** directory schema.

---

## 🚨 Troubleshooting & Recovery Playbook

| Issue / Failure | Root Cause | Agent Action Plan |
| :--- | :--- | :--- |
| **ASR engine throws DLL load warning** | ONNX Runtime GPU provider conflicts. | Fall back to CPU provider programmatically. Re-init ASR configuration with `provider: Some("cpu")`. |
| **VAD hangs/CPU thrashes during tests** | Inference overhead on unoptimized debug builds. | Warm up the VAD Cache session (`VAD_CACHE`) or run tests with `--release` flags to compile neural dependencies with full compiler optimization. |
| **SQLite database returns "database is locked"** | Concurrent thread write conflict. | Ensure SQLite runs in WAL mode. Enable busy timeout loops (default `5000ms`) and retry transactions. |
| **Aligner returns proportional heuristics** | Model mismatch or energy fallback triggered. | Check if forced aligner model directory exists. If fallback is active, warn the user that timestamps are energy-approximations. |
