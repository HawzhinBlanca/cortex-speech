#!/usr/bin/env python3
"""Build one per-dialect manifest from CORDI for the dialect-fairness leg (charter item 53).

Writes `<clip_path>\\t<reference>` TSVs — the format `scorecard_7b.py` and `scorecard_finetuned.py`
already consume — so the CER, the seed-42 bootstrap CI and the normalization are the SAME code that
produced every other measured number in this repo. Inventing a second CER path here would make the
dialect numbers non-comparable with the 7.03% headline, which is the whole point of measuring them.

CORDI ships one `.ogg` per utterance and labels DIALECT per film (not per utterance), so the label is
joined from `metadata.tsv` by film id.

What it refuses to include, because each would quietly corrupt a dialect's CER:
  * `is_synched = FALSE` films — audio and text are not aligned, so the reference does not describe
    the audio and the resulting CER measures nothing.
  * missing audio files (`Video not available` covers 5 films).
  * empty references, and clips outside a sane duration band.

Usage:
    python scripts/build_cordi_manifest.py --cordi <dir> [--per-dialect 40] [--out <dir>] [--wsl]
"""

from __future__ import annotations

import argparse
import csv
import json
import random
import sys
import unicodedata
from pathlib import Path

# Dialect names are Kurdish Latin (Silêmanî, Serdeşt): a cp1252 stdout dies on them mid-report, after
# some manifests are already on disk. Measured 2026-08-11 — the run looked like a partial success.
sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def ascii_slug(name: str) -> str:
    """Filename-safe id. The manifests are handed to WSL and to shell tooling; keep the pretty name
    for the REPORT and an ASCII one for the path."""
    stripped = unicodedata.normalize("NFKD", name).encode("ascii", "ignore").decode()
    return "".join(c if c.isalnum() else "_" for c in stripped).strip("_") or "unknown"

MIN_MS, MAX_MS = 1000, 15000
SEED = 42


def to_wsl(path: Path) -> str:
    text = str(path).replace("\\", "/")
    if len(text) > 1 and text[1] == ":":
        return f"/mnt/{text[0].lower()}{text[2:]}"
    return text


def load_dialects(metadata: Path) -> dict[str, str]:
    """film id -> dialect, excluding unsynched films."""
    mapping: dict[str, str] = {}
    dropped = 0
    with metadata.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            if (row.get("is_synched") or "").strip().upper() == "FALSE":
                dropped += 1
                continue
            dialect = (row.get("Dialect") or "").strip()
            film = (row.get("ID") or "").strip()
            if dialect and film:
                mapping[film] = dialect
    print(f"films: {len(mapping)} usable, {dropped} dropped as not synched")
    return mapping


def collect(cordi: Path, dialects: dict[str, str]) -> dict[str, list[tuple[Path, str]]]:
    by_dialect: dict[str, list[tuple[Path, str]]] = {}
    missing_audio = 0
    for film, dialect in sorted(dialects.items()):
        transcript = cordi / "segments" / f"{film}.json"
        if not transcript.is_file():
            continue
        try:
            items = json.loads(transcript.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        for item in items:
            text = (item.get("text") or "").strip()
            if not text:
                continue
            try:
                duration = float(item["end"]) - float(item["start"])
            except (KeyError, TypeError, ValueError):
                continue
            if not MIN_MS <= duration <= MAX_MS:
                continue
            rel = (item.get("audio") or "").replace("content/", "", 1)
            clip = cordi / rel
            if not clip.is_file():
                missing_audio += 1
                continue
            by_dialect.setdefault(dialect, []).append((clip, text))
    if missing_audio:
        print(f"skipped {missing_audio} utterance(s) whose audio file is absent")
    return by_dialect


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cordi", type=Path, required=True, help="extracted CORDI dir (holds segments/)")
    parser.add_argument("--metadata", type=Path, default=None, help="metadata.tsv (default: <cordi>/metadata.tsv)")
    parser.add_argument("--per-dialect", type=int, default=40)
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--wsl", action="store_true", help="emit /mnt/... paths for the WSL-hosted 7B")
    args = parser.parse_args()

    metadata = args.metadata or (args.cordi / "metadata.tsv")
    out_dir = args.out or (args.cordi / "manifests")
    out_dir.mkdir(parents=True, exist_ok=True)

    by_dialect = collect(args.cordi, load_dialects(metadata))
    if not by_dialect:
        print("no usable utterances found — refusing to write empty manifests")
        return 2

    rng = random.Random(SEED)
    written = []
    for dialect, pool in sorted(by_dialect.items(), key=lambda kv: -len(kv[1])):
        sample = pool if len(pool) <= args.per_dialect else rng.sample(pool, args.per_dialect)
        path = out_dir / f"cordi_{ascii_slug(dialect)}.tsv"
        with path.open("w", encoding="utf-8", newline="") as handle:
            for clip, text in sample:
                location = to_wsl(clip) if args.wsl else str(clip)
                handle.write(f"{location}\t{text}\n")
        written.append((dialect, len(sample), len(pool), path))
        print(f"  {dialect:12} {len(sample):4} sampled of {len(pool):6} available -> {path.name}")

    thin = [d for d, n, _, _ in written if n < 30]
    if thin:
        print(f"\nNOTE: {thin} have fewer than 30 clips. A CER over so few utterances carries a "
              "confidence interval too wide to call a disparity — report N beside every number.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
