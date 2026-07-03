#!/usr/bin/env python3
"""P2.1 — build a frozen Common Voice 22 ckb eval manifest for the M1 engine decision.

Emits the TSV the scorecards already parse — one `<audio_path>\t<reference>` row per clip — from the
on-disk CV22 ckb release, plus a `.sha256` sidecar so the eval set is pinned. CV22 ships audio as a
per-split tar shard (`audio/ckb/<split>/ckb_<split>_N.tar`), so pass `--extract` once to unpack it
next to the tsv before building; without it the builder assumes the mp3s are already extracted.

    python scripts/build_cv22_ckb_manifest.py --cv22-dir <DIR> --split test --extract \
        --output scripts/cv22_ckb_test_frozen.tsv

`--wsl-paths` rewrites C:\\... to /mnt/c/... so the manifest is consumable by scorecard_7b.py, which
runs inside WSL. References are emitted RAW (tabs/newlines sanitized to spaces); the scorecards apply
their own identical NFC+lower+whitespace normalization to hyp and ref, so the CER/WER stay comparable
to the published numbers. Nothing is fabricated — a row is written only when its audio file exists.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import os
import sys
import tarfile
from pathlib import Path


def to_wsl_path(p: str) -> str:
    """C:\\Users\\x -> /mnt/c/Users/x (idempotent for already-POSIX paths)."""
    p = p.replace("\\", "/")
    if len(p) >= 2 and p[1] == ":":
        drive = p[0].lower()
        rest = p[2:].lstrip("/")
        return f"/mnt/{drive}/{rest}"
    return p


def sanitize_reference(text: str) -> str:
    """Collapse any tab/newline (TSV-breaking) to spaces; trim. Content is otherwise left RAW."""
    return " ".join((text or "").replace("\t", " ").replace("\n", " ").split())


def find_columns(header: list[str]) -> tuple[int, int]:
    """Locate the `path` and `sentence` columns by name, robust to column reordering."""
    lower = [h.strip().lower() for h in header]
    try:
        return lower.index("path"), lower.index("sentence")
    except ValueError as exc:
        raise ValueError(f"CV22 tsv header missing 'path'/'sentence' columns: {header}") from exc


def extract_tars(audio_dir: Path) -> int:
    """Extract any *.tar shard in `audio_dir` into it (members flattened). Returns files extracted."""
    extracted = 0
    for tar_path in sorted(audio_dir.glob("*.tar")):
        with tarfile.open(tar_path) as tf:
            for member in tf.getmembers():
                if not member.isfile():
                    continue
                # Flatten: write each member as audio_dir/<basename>, never outside audio_dir.
                name = os.path.basename(member.name)
                if not name:
                    continue
                dest = audio_dir / name
                with tf.extractfile(member) as src:  # type: ignore[union-attr]
                    if src is None:
                        continue
                    dest.write_bytes(src.read())
                extracted += 1
    return extracted


def iter_manifest_rows(tsv_path: Path, audio_dir: Path, limit: int | None, wsl: bool):
    """Yield (audio_path, reference) for each tsv row whose audio file exists. Also returns skipped."""
    skipped = 0
    emitted = 0
    with open(tsv_path, encoding="utf-8", newline="") as fh:
        reader = csv.reader(fh, delimiter="\t")
        header = next(reader)
        path_idx, sentence_idx = find_columns(header)
        for row in reader:
            if limit is not None and emitted >= limit:
                break
            if len(row) <= max(path_idx, sentence_idx):
                skipped += 1
                continue
            audio_file = audio_dir / row[path_idx].strip()
            reference = sanitize_reference(row[sentence_idx])
            if not audio_file.is_file() or not reference:
                skipped += 1
                continue
            out_path = to_wsl_path(str(audio_file.resolve())) if wsl else str(audio_file.resolve())
            yield out_path, reference
            emitted += 1
    yield ("__skipped__", str(skipped))  # sentinel trailer so callers learn the skip count


def build(cv22_dir: Path, split: str, output: Path, limit: int | None, wsl: bool, extract: bool) -> tuple[int, int]:
    tsv_path = cv22_dir / "transcript" / "ckb" / f"{split}.tsv"
    audio_dir = cv22_dir / "audio" / "ckb" / split
    if not tsv_path.is_file():
        raise FileNotFoundError(f"CV22 transcript not found: {tsv_path}")
    if not audio_dir.is_dir():
        raise FileNotFoundError(f"CV22 audio dir not found: {audio_dir}")

    if extract:
        n = extract_tars(audio_dir)
        print(f"extracted {n} audio files from tar shards in {audio_dir}", file=sys.stderr)

    rows: list[tuple[str, str]] = []
    skipped = 0
    for a, b in iter_manifest_rows(tsv_path, audio_dir, limit, wsl):
        if a == "__skipped__":
            skipped = int(b)
            break
        rows.append((a, b))

    output.parent.mkdir(parents=True, exist_ok=True)
    with open(output, "w", encoding="utf-8", newline="") as out:
        for audio_path, reference in rows:
            out.write(f"{audio_path}\t{reference}\n")

    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_suffix(output.suffix + ".sha256").write_text(f"{digest}  {output.name}\n", encoding="utf-8")
    return len(rows), skipped


def main() -> int:
    ap = argparse.ArgumentParser(description="Build a frozen CV22 ckb eval manifest (TSV).")
    ap.add_argument("--cv22-dir", default=os.environ.get("CORTEX_CV22_DIR", ""), help="CV22 release root")
    ap.add_argument("--split", default="test", choices=["test", "dev", "train"])
    ap.add_argument("--output", default="scripts/cv22_ckb_test_frozen.tsv")
    ap.add_argument("--limit", type=int, default=None, help="cap the number of clips (default: all)")
    ap.add_argument("--wsl-paths", action="store_true", help="rewrite C:\\ paths to /mnt/c/ for WSL scorecards")
    ap.add_argument("--extract", action="store_true", help="unpack the split's *.tar audio shard first")
    args = ap.parse_args()

    if not args.cv22_dir:
        print("error: --cv22-dir (or CORTEX_CV22_DIR) is required", file=sys.stderr)
        return 2

    n, skipped = build(
        Path(args.cv22_dir), args.split, Path(args.output), args.limit, args.wsl_paths, args.extract
    )
    print(f"wrote {n} rows to {args.output} ({skipped} skipped: missing audio or empty reference)")
    if n == 0:
        print("WARNING: 0 rows — did you need --extract to unpack the tar shard?", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
