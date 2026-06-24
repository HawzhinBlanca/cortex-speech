#!/usr/bin/env python3
"""Build a small seed-fixed Sorani gold manifest from the user's local corpus for CER measurement.

Extracts N real (incompressible — not a zero placeholder) audio clips from chunk_7.zip, transcodes
each to 16 kHz mono WAV via ffmpeg, looks up its verified Sorani reference in the transcription TSV,
and writes a manifest: <wav_path>\t<reference>. Eval-only; nothing is committed.
"""
import os
import random
import subprocess
import sys
import tempfile
import zipfile

# Corpus locations come from the environment (no private paths committed):
#   CORTEX_CORPUS_ZIP   audio chunk archive (e.g. chunk_7.zip)
#   CORTEX_CORPUS_TSV   transcription TSV (cols: <basename>\t<sentence>\t...)
#   FFMPEG              ffmpeg executable (default: "ffmpeg" on PATH)
#   CORTEX_GOLD_OUT     output dir (default: <tmp>/cortex_gold)
ZIP = os.environ.get("CORTEX_CORPUS_ZIP", "")
TSV = os.environ.get("CORTEX_CORPUS_TSV", "")
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")
OUT = os.environ.get("CORTEX_GOLD_OUT", os.path.join(tempfile.gettempdir(), "cortex_gold"))
N = int(sys.argv[1]) if len(sys.argv) > 1 else 50
SEED = int(sys.argv[2]) if len(sys.argv) > 2 else 42

if not ZIP or not TSV:
    sys.exit("set CORTEX_CORPUS_ZIP and CORTEX_CORPUS_TSV to the corpus archive + transcription TSV")


def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    zf = zipfile.ZipFile(ZIP)
    real = [
        zi for zi in zf.infolist()
        if not zi.is_dir()
        and zi.filename.lower().endswith((".mp3", ".wav", ".m4a", ".ogg"))
        and zi.file_size > 8000
        and zi.compress_size >= 0.85 * zi.file_size  # incompressible => real audio, not a placeholder
    ]
    random.seed(SEED)
    random.shuffle(real)
    print(f"{len(real)} real audio entries in archive; sampling {N} (seed={SEED})")

    # Reference lookup: basename(no ext) -> sentence. Load the TSV once.
    refs = {}
    with open(TSV, encoding="utf-8", errors="ignore") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 2:
                refs[os.path.splitext(parts[0])[0]] = parts[1]
    print(f"loaded {len(refs)} references")

    manifest = os.path.join(OUT, "gold_manifest.tsv")
    written = 0
    with open(manifest, "w", encoding="utf-8") as mf:
        for zi in real:
            if written >= N:
                break
            stem = os.path.splitext(os.path.basename(zi.filename))[0]
            ref = refs.get(stem)
            if not ref:
                continue
            raw = os.path.join(OUT, "src" + os.path.splitext(zi.filename)[1])
            with zf.open(zi) as s, open(raw, "wb") as d:
                d.write(s.read())
            wav = os.path.join(OUT, f"{stem}.wav")
            r = subprocess.run([FFMPEG, "-y", "-i", raw, "-ac", "1", "-ar", "16000", wav],
                               capture_output=True, text=True)
            if r.returncode != 0 or not os.path.exists(wav):
                continue
            mf.write(f"{wav}\t{ref}\n")
            written += 1
    print(f"wrote {written} clips -> {manifest}")
    return 0 if written else 1


if __name__ == "__main__":
    sys.exit(main())
