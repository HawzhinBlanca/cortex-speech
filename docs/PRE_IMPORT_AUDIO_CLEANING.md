# Pre-import audio cleaning, and how the library stays honest about it

**Status:** contract, 2026-08-17. Not canon — this describes how an existing external tool connects
to Cortex, and may evolve. The one part that must not be weakened is the provenance rule below.

## What the cleaner is, and why it is not in the app

`kurdish-audio-cleaner` (separate repo, `HawzhinBlanca/kurdish-audio-cleaner`) prepares web-sourced
audio before Cortex ever sees it:

1. **MelBand-RoFormer** separates voice from everything else and keeps only the vocal stem;
2. **Silero VAD** cuts out music intros, interludes and dead air, re-concatenating what survives
   with 150 ms pauses;
3. the result is resampled to 24 kHz, high-passed at 40 Hz, and normalised to −20 LUFS.

It stays a separate tool on purpose:

- it is PyTorch + CUDA + `audio-separator`, several GB. Cortex is a self-contained Rust binary that
  bundles ONNX models. Embedding it means shipping a Python environment in the installer or porting
  MelBand-RoFormer to ONNX — a project, not an integration.
- it is inherently a **pre-import** step. Because it CUTS audio out, a cleaned file's timeline no
  longer maps to the source, so it can only ever run before the app knows about the recording.
- Cortex's own GTCRN is a speech **denoiser**, a different class of model. The two do not overlap and
  neither replaces the other.

## The provenance rule (do not weaken)

A cleaned file is an ordinary WAV. Nothing about it says a neural model removed most of what was
recorded. Once its clips are in the library, an export would describe machine-separated,
re-concatenated, loudness-normalised audio in exactly the words used for the owner's own microphone
captures. **That is a provenance lie, and it is the reason this contract exists.**

So:

- the cleaner writes a `cleaning` block into its `manifest.json` (`MANIFEST_PROVENANCE`), stating the
  separator model, what was removed, that the timeline was cut, and the resample/loudness targets;
- `src-tauri/src/source_provenance.rs` finds that manifest when a file is imported (searching up to
  6 parent levels, since the cleaner mirrors the source tree) and turns it into a declaration;
- `process_single_file_with_progress` records it in `source_audio_provenance` (migration v54), keyed
  by source path, **before** anything is decoded;
- `export_dataset` prints one `processed_audio` notice per affected recording, with the clip count it
  actually contributed to that export.

Detection is deliberately conservative and reads only an explicit `audio_is_processed: true`. It
never infers processing from a directory name: a false positive brands original recordings as
processed, and a false negative is the exact lie this prevents.

**An absent record means "unclaimed", never "verified original."** A recording imported before v54,
or processed by some other tool that left no manifest, makes no claim either way — which is why the
export states what is known rather than asserting the rest of the corpus is raw.

Enforced by `scripts/test_processed_audio_provenance_policy.py` (schema → import → export).

## Operating notes

- The cleaner outputs 24 kHz; Cortex resamples to 16 kHz on import. That is right if the corpus is
  also for TTS, and wasted work if it is only for this app.
- Never re-clean a recording the library already holds, and never import both the raw and cleaned
  versions of the same recording: the audio differs, so duplicate detection may not catch the pair,
  and their timelines disagree.
- The manifest for material cleaned before 2026-08-17 was backfilled with the `cleaning` block, since
  those files came from this same pipeline with these same parameters. A `.pre-provenance-backup`
  copy sits beside it.
