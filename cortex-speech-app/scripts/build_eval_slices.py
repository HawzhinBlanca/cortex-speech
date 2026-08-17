#!/usr/bin/env python3
"""Build the PROTECTED SLICES file the promotion gate checks a challenger against.

    python scripts/build_eval_slices.py <gold_manifest.tsv> [--out slices.tsv]

`promotion_gate.py` refuses to promote a challenger that regresses on any protected group. That rule
is the audit's own argument made mechanical: with 94.7 % of labeled duration coming from ONE
recording, an overall CER average IS that one voice, and a headline win can hide real harm to a
dialect, a speaker, or the noisy clips. But a rule needs groups, and nothing produced them — so the
gate's most valuable check was unusable. This writes them.

Output is `slice_name<TAB>clip_index`, where clip_index is the manifest's ROW INDEX — the same key
the scorecards emit, so the gate can pair everything without a second lookup.

Slices come from what the library actually knows about each clip:

  * dialect:<name>  — from `dialect.rs`'s SOURCE_DIALECTS map, the same source of truth the reviewer
    queue uses to decide who may judge what. A reviewer who does not speak a dialect must not judge
    it; a model that regresses on one must not be promoted for it.
  * speaker:<id>    — per-recording diarizer labels are NOT identities, so speakers are keyed by
    RECORDING, which is the closest thing to a voice this library can honestly assert.
  * audio:noisy / audio:clean — split on measured SNR. A model that improves on clean speech while
    degrading on the difficult clips has not improved for the recordings that need it most.

A manifest row the library does not recognise is reported and left OUT of every slice rather than
guessed into one: an inferred group is worse than a missing one, because the gate would then be
protecting something that is not there.
"""

from __future__ import annotations

import argparse
import os
import re
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
# Below this measured SNR a clip counts as difficult audio. 15 dB is the same neighbourhood the
# quality rubric treats as degraded; the exact value matters less than having the bucket at all.
NOISY_SNR_DB = 15.0


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def source_dialects() -> list[tuple[str, str]]:
    """The SOURCE_DIALECTS table, parsed out of dialect.rs so the two can never drift."""
    dialect_rs = (REPO_ROOT / "src-tauri/src/dialect.rs").read_text(encoding="utf-8")
    consts = dict(re.findall(r'pub const ([A-Z_]+): &str = "([a-z-]+)";', dialect_rs))
    block = re.search(r"const SOURCE_DIALECTS:[^=]*=\s*&\[(.*?)\n\];", dialect_rs, re.S)
    if not block:
        raise SystemExit("could not find SOURCE_DIALECTS in dialect.rs — this generator needs updating")
    pairs: list[tuple[str, str]] = []
    for fragment, name in re.findall(r'\(\s*r?"((?:[^"\\]|\\.)*)"\s*,\s*([A-Za-z_]+)\s*\)', block.group(1)):
        dialect = consts.get(name, name.lower())
        pairs.append((fragment.replace("\\\\", "\\"), dialect))
    return pairs


def dialect_of(audio_path: str, table: list[tuple[str, str]]) -> str | None:
    normalized = audio_path.replace("/", "\\")
    for fragment, dialect in table:
        if fragment and fragment in normalized:
            return dialect
    return None


def library_index() -> dict[str, tuple[str, float | None]]:
    """basename -> (source recording path, snr_db) for every clip the library holds."""
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        return {}
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = con.execute("SELECT id, audio_path, snr_db FROM speech_segments").fetchall()
    finally:
        con.close()
    index: dict[str, tuple[str, float | None]] = {}
    for seg_id, path, snr in rows:
        # Exported eval clips are named by segment id; source clips match on their own basename.
        index[seg_id] = (path, snr)
        index[os.path.basename(path)] = (path, snr)
    return index


def build(manifest_rows: list[str], index, dialects) -> tuple[dict[str, list[int]], list[int]]:
    slices: dict[str, list[int]] = defaultdict(list)
    unrecognised: list[int] = []
    for row_index, line in enumerate(manifest_rows):
        clip_path = line.split("\t")[0].strip()
        if not clip_path:
            continue
        stem = Path(clip_path).stem
        base = os.path.basename(clip_path)
        entry = index.get(stem) or index.get(base)
        if entry is None:
            unrecognised.append(row_index)
            continue
        source, snr = entry
        dialect = dialect_of(source, dialects)
        if dialect:
            slices[f"dialect:{dialect}"].append(row_index)
        slices[f"speaker:{os.path.basename(source)}"].append(row_index)
        if snr is not None:
            slices["audio:noisy" if snr < NOISY_SNR_DB else "audio:clean"].append(row_index)
    return slices, unrecognised


def main() -> int:
    parser = argparse.ArgumentParser(description="Protected slices for the promotion gate")
    parser.add_argument("manifest", type=Path, help="the eval manifest the scorecards were run on")
    parser.add_argument("--out", type=Path)
    parser.add_argument(
        "--min-clips",
        type=int,
        default=5,
        help="drop slices smaller than this — a 2-clip 'group' measures nothing and would block "
        "every promotion on noise",
    )
    args = parser.parse_args()

    rows = args.manifest.read_text(encoding="utf-8").splitlines()
    slices, unrecognised = build(rows, library_index(), source_dialects())

    kept = {name: clips for name, clips in slices.items() if len(clips) >= args.min_clips}
    dropped = {name: len(clips) for name, clips in slices.items() if len(clips) < args.min_clips}

    out = args.out or args.manifest.with_name("eval_slices.tsv")
    with out.open("w", encoding="utf-8") as handle:
        handle.write("# slice_name\tclip_index — generated by build_eval_slices.py\n")
        for name in sorted(kept):
            for clip in sorted(kept[name]):
                handle.write(f"{name}\t{clip}\n")

    print(f"EVAL SLICES: {len(kept)} protected group(s) over {len(rows)} manifest row(s)", flush=True)
    for name in sorted(kept):
        print(f"  {name:34s} {len(kept[name]):5d} clips", flush=True)
    if dropped:
        # Said out loud, never silently: a group that vanished is a group nobody is protecting.
        print(f"  dropped {len(dropped)} slice(s) under --min-clips {args.min_clips}: "
              f"{', '.join(sorted(dropped))}", flush=True)
    if unrecognised:
        print(f"  WARNING: {len(unrecognised)} manifest row(s) matched no library clip — left out of "
              f"every slice rather than guessed into one", flush=True)
    print(f"  written to {out}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
