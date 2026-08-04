#!/usr/bin/env python3
"""P2.1 — build a frozen FLEURS ckb_IQ eval manifest for the M1 engine decision.

Downloads the google/fleurs ckb_IQ **test** split, writes each clip as a 16 kHz mono WAV under
`clips/`, and emits the SAME `<wav_path>\t<reference>` TSV the scorecards parse (+ `.sha256` sidecar).
This is the set that gives a FLEURS-ckb number directly comparable to ElevenLabs Scribe's published
32.1% WER (theirs is WER on FLEURS-ckb; report the CI — FLEURS ckb_IQ test is only ~350 sentences).

    python scripts/build_fleurs_ckb_manifest.py --output-dir scripts/fleurs_ckb_iq --wsl-paths

The HuggingFace download (~1–2 GB, `pip install datasets soundfile`) runs on the owner's machine; the
clip-writing/manifest core (`write_manifest`) is unit-tested with synthetic examples. A row is written
only when its audio array and reference are both present — nothing is fabricated. FLEURS is already
16 kHz mono; `transcription` is the reference (the scorecards apply their own normalization).
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import pathlib
from pathlib import Path

# Reuse the CV22 builder's tested path/reference helpers (same scripts/ dir).
from build_cv22_ckb_manifest import sanitize_reference, to_wsl_path

MANIFEST_NAME = "fleurs_ckb_iq_frozen.tsv"


def write_manifest(examples, out_dir: Path, wsl: bool = False, limit: int | None = None) -> tuple[int, int]:
    """Write clips + manifest from an iterable of FLEURS-shaped examples.

    Each example is a mapping with `audio` = {"array": <float samples>, "sampling_rate": int} and a
    `transcription` (falls back to `raw_transcription`). Returns (rows_written, skipped).
    """
    import soundfile as sf  # local import so the module stays importable without the audio dep

    clips_dir = out_dir / "clips"
    clips_dir.mkdir(parents=True, exist_ok=True)

    rows: list[tuple[str, str]] = []
    seen_ids: dict[str, int] = {}
    skipped = 0
    for i, ex in enumerate(examples):
        if limit is not None and len(rows) >= limit:
            break
        audio = ex.get("audio") or {}
        array = audio.get("array")
        sample_rate = int(audio.get("sampling_rate") or 16000)
        reference = sanitize_reference(ex.get("transcription") or ex.get("raw_transcription") or "")
        if array is None or len(array) == 0 or not reference:
            skipped += 1
            continue
        # FLEURS `id` is the SENTENCE id, shared across multiple recordings of the same sentence.
        # Naming every same-id clip `<id>.wav` clobbers the audio on disk AND emits exact-duplicate
        # manifest rows (identical path + reference), which the scorecards count as separate clips —
        # inflating N and duplication-weighting micro-CER. Suffix each recording after the first with
        # `.<n>` so every row maps to a distinct clip (guarded by test_frozen_eval_manifest_integrity).
        clip_id = str(ex.get("id", i))
        dup = seen_ids.get(clip_id, 0)
        seen_ids[clip_id] = dup + 1
        clip_name = clip_id if dup == 0 else f"{clip_id}.{dup}"
        clip_path = clips_dir / f"{clip_name}.wav"
        sf.write(str(clip_path), array, sample_rate, subtype="PCM_16")
        out_path = to_wsl_path(str(clip_path.resolve())) if wsl else str(clip_path.resolve())
        rows.append((out_path, reference))

    manifest_path = out_dir / MANIFEST_NAME
    with open(manifest_path, "w", encoding="utf-8", newline="") as f:
        for out_path, reference in rows:
            f.write(f"{out_path}\t{reference}\n")
    digest = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    manifest_path.with_suffix(manifest_path.suffix + ".sha256").write_text(
        f"{digest}  {manifest_path.name}\n", encoding="utf-8"
    )
    return len(rows), skipped


def _decode_with_soundfile(ds):
    """Yield FLEURS-shaped examples, decoding the audio with soundfile instead of datasets' decoder.

    `datasets` >= 4 decodes an Audio column through **torchcodec**, and raises
    `ImportError: To support decoding audio data, please install 'torchcodec'` otherwise — which pulls a
    full PyTorch runtime (GBs) purely to turn a 16 kHz WAV into samples. Measured 2026-08-04: this is
    what stopped the one-time fetch dead after the download had already completed.

    soundfile is ALREADY a hard dependency here (`write_manifest` writes the clips with it), so the
    bytes are decoded in-process and `write_manifest` — the unit-tested core — is handed exactly the
    shape it documents. Nothing about the corpus changes; only who decodes it.
    """
    import io

    import soundfile as sf

    for ex in ds:
        audio = ex.get("audio") or {}
        raw = audio.get("bytes")
        if raw is None and audio.get("path"):
            raw = pathlib.Path(audio["path"]).read_bytes()
        if not raw:
            # An example with no audio is SKIPPED by write_manifest (and counted), never fabricated.
            yield {**ex, "audio": {}}
            continue
        array, sample_rate = sf.read(io.BytesIO(raw), dtype="float32")
        yield {**ex, "audio": {"array": array, "sampling_rate": int(sample_rate)}}


def main() -> int:
    ap = argparse.ArgumentParser(description="Build a frozen FLEURS ckb_IQ eval manifest (TSV).")
    ap.add_argument("--output-dir", default="scripts/fleurs_ckb_iq", help="dir for clips/ + the manifest")
    ap.add_argument("--config", default="ckb_iq", help="FLEURS config/locale (default ckb_iq)")
    ap.add_argument("--split", default="test", choices=["test", "dev", "train"])
    ap.add_argument("--limit", type=int, default=None, help="cap the number of clips (default: all)")
    ap.add_argument("--wsl-paths", action="store_true", help="rewrite C:\\ paths to /mnt/c/ for WSL scorecards")
    args = ap.parse_args()

    try:
        from datasets import Audio, load_dataset
    except ImportError:
        print("error: `pip install datasets soundfile` to build the FLEURS manifest", file=sys.stderr)
        return 2

    print(f"downloading google/fleurs {args.config} {args.split} (~1-2 GB, one-time)…", file=sys.stderr)
    ds = load_dataset("google/fleurs", args.config, split=args.split)
    # Hand back the RAW bytes and decode them ourselves — see _decode_with_soundfile.
    ds = ds.cast_column("audio", Audio(decode=False))
    out_dir = Path(args.output_dir)
    n, skipped = write_manifest(_decode_with_soundfile(ds), out_dir, wsl=args.wsl_paths, limit=args.limit)
    print(f"wrote {n} rows to {out_dir / MANIFEST_NAME} ({skipped} skipped: empty audio or reference)")
    if n == 0:
        print("WARNING: 0 rows — check the FLEURS config/split", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
