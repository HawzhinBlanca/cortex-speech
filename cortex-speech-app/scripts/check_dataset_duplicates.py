#!/usr/bin/env python3
"""The same-recording-under-different-names audit, on the LIVE library.

FOUND BY THE OWNER'S EARS, 2026-08-17, not by any gate — which is why this exists. The library held
one recording under THREE filenames (Lamofull2_00086400_A01 / Lamofull00086400_A01 / _A02): the
files are different ENCODES, so the byte-level audio fingerprint (v50/51) sees three distinct files,
and every clip cut from them imported as new work. ~65 duplicate sentences entered the corpus, 33 of
them were REVIEWED TWICE (paid twice), and the same content in nominally-different recordings can
straddle a train/test split — silent leakage that invalidates any measurement taken across it.

THE SIGNAL: two clips from DIFFERENT files whose source-timeline offset AND champion transcript both
match. Offsets are positions on the recording's own clock, so two files agreeing about where the
same sentence sits is the same recording — a repeated phrase in genuinely different recordings does
not sit at the same millisecond. Text under 25 chars is ignored (short interjections repeat by
chance); offsets are bucketed to 500 ms (encoder padding shifts).

Exit 1 when duplicate content EXCEEDS the recorded baseline (a new duplicate import happened);
otherwise reports the count, which must only ever go DOWN. Baseline ratchets to 0 after the cleanup.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path

# The duplicates that existed the day this gate was written, awaiting the owner-gated cleanup.
# After the cleanup, set to 0 — from then on a single new duplicate is a RED sweep.
KNOWN_BASELINE = 70
MIN_TEXT_CHARS = 25
OFFSET_BUCKET_MS = 500


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def duplicate_groups(rows: list[tuple[str, str, str, str, int]]) -> list[list[tuple[str, str]]]:
    """Groups of (segment_id, source_file) sharing offset+text across DIFFERENT files.

    `rows` = (id, audio_path, alignment_json, raw_transcript, verified).
    Pure so test_dataset_duplicates.py can pin it without a database.
    """
    # Grouped by TEXT, then clustered by offset distance — never by a floor-bucket, whose edges split
    # a genuine 13 ms encoder-padding difference into two buckets exactly often enough to miss real
    # duplicates (caught by this module's own unit test before it ever ran on the library).
    by_text: dict[str, list[tuple[int, str, str]]] = defaultdict(list)
    for seg_id, path, alignment_json, raw, _verified in rows:
        text = (raw or "").strip()
        if len(text) < MIN_TEXT_CHARS:
            continue
        try:
            offset = int(json.loads(alignment_json or "{}").get("source_start_ms", -1))
        except (ValueError, TypeError):
            continue
        if offset < 0:
            continue
        by_text[text].append((offset, seg_id, os.path.basename(path)))
    out = []
    for entries in by_text.values():
        entries.sort()
        cluster: list[tuple[int, str, str]] = []
        for entry in entries:
            if cluster and entry[0] - cluster[-1][0] > OFFSET_BUCKET_MS:
                if len({f for _, _, f in cluster}) > 1:
                    out.append(sorted((sid, f) for _, sid, f in cluster))
                cluster = []
            cluster.append(entry)
        if cluster and len({f for _, _, f in cluster}) > 1:
            out.append(sorted((sid, f) for _, sid, f in cluster))
    return sorted(out)


def main() -> int:
    db = _data_dir() / "cortex-speech.db"
    if not db.is_file():
        print(f"DATASET DUPLICATES: SKIP-ENV (no library at {db})", flush=True)
        return 0
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT id, audio_path, alignment_json, raw_transcript, verified FROM speech_segments"
        ).fetchall()
    finally:
        con.close()

    groups = duplicate_groups(rows)
    redundant = sum(len(g) - 1 for g in groups)  # each group needs all but one removed

    if redundant > KNOWN_BASELINE:
        print("DATASET DUPLICATES: FAIL", flush=True)
        print(
            f"  {redundant} redundant clips across {len(groups)} duplicate-content groups — ABOVE the "
            f"recorded baseline of {KNOWN_BASELINE}, so a duplicate recording has been imported since "
            f"this gate was written. Same recording, different encode: the byte fingerprint cannot see "
            f"it; this offset+text audit can.",
            flush=True,
        )
        by_file: dict[frozenset, int] = defaultdict(int)
        for g in groups:
            by_file[frozenset(f for _, f in g)] += 1
        for files, n in sorted(by_file.items(), key=lambda kv: -kv[1])[:10]:
            print(f"    {n:4} groups across: {', '.join(sorted(files))}", flush=True)
        return 1

    if redundant:
        print(
            f"DATASET DUPLICATES: OK-WITH-BASELINE ({redundant} known redundant clips, baseline "
            f"{KNOWN_BASELINE} — the owner-gated cleanup will ratchet this to 0; the count must only "
            f"ever go DOWN)",
            flush=True,
        )
    else:
        print("DATASET DUPLICATES: OK (no cross-file duplicate content)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
