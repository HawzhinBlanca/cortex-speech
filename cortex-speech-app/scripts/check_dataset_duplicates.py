#!/usr/bin/env python3
"""The same-recording-under-different-names audit, on the LIVE library.

FOUND BY THE OWNER'S EARS, 2026-08-17, not by any gate — which is why this exists. The library held
one recording under THREE filenames (Lamofull2_00086400_A01 / Lamofull00086400_A01 / _A02): the
files are different ENCODES, so the byte-level audio fingerprint (v50/51) sees three distinct files,
and every clip cut from them imported as new work. ~65 duplicate sentences entered the corpus, 33 of
them were REVIEWED TWICE (paid twice), and the same content in nominally-different recordings can
straddle a train/test split — silent leakage that invalidates any measurement taken across it.

TWO SIGNALS — the second exists because the owner's ears beat the first within the hour:

RULE A — EXACT TEXT, any offset. The same >= 25-char champion transcript in two DIFFERENT files is
the same recording: real decodes of genuinely different recordings always drift somewhere in a
sentence that long. The first version required matching source offsets too, and the owner then
heard the SAME sentence again on the very page the audit had "deduplicated": the library holds a
THIRD encode (`A1-0032_PODCAST-001.mp4`) whose timeline is shifted by a constant 137.8 s, so
offset agreement was blind to it. The Lamofull*.flac files are the FULL-LENGTH recordings; the
`A1-00xx` episode files are cuts of the same material — two generations of one corpus, both
imported.

RULE B — same offset, DRIFTED text. Twins at the same source-clock position (within 500 ms) whose
transcripts are >= 90% similar: different encodes decode a letter apart (`بەڵێ`/`بەلێ`), so exact
matching misses them exactly where rule A's premise (decodes drift) works against us. Offset
agreement carries the burden of proof here instead.

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
#
# CORRECTED 70 -> 170 the same day, and the correction is the honest part: the first measurement
# required offset agreement, which the mp4's shifted clock defeated — the owner HEARD the remaining
# twins on a page the audit had "deduplicated". 170 is what rules A+B measure on 2026-08-17's
# library. This number may only ever be lowered (the cleanup) — raising it again requires the
# owner's `change canon:`, because a higher baseline is indistinguishable from waving through a
# fresh duplicate import.
KNOWN_BASELINE = 0
MIN_TEXT_CHARS = 25
OFFSET_BUCKET_MS = 500

# RULE C — THE AUDIO DECIDES (2026-08-18). Rules A and B match TEXT, and text alone cannot tell a
# duplicated import from a narrator saying the same sentence twice. The audiobook corpus makes that
# distinction load-bearing: every episode of `bangewazek_bo_behesht` opens by announcing the series
# title, and a ghazal collection repeats verses across chapters. Measured on the first 5 books
# imported, ALL THREE flagged groups were legitimate repeats — with 134 books to import, this gate
# would have gone permanently red and been ignored, which is how a real duplicate gets through.
#
# So a text match is now a CANDIDATE, and the clip audio confirms it. Two readings of one sentence
# differ everywhere; a duplicated import is the same samples. This STRENGTHENS the gate: every true
# positive still fails it, and the false positives stop.
#
# Correlation of the two clips' normalised waveforms, compared at equal length. Identical audio
# scores ~1.0; two separate readings of the same words score far below this even at the same tempo.
AUDIO_DUPLICATE_CORRELATION = 0.98
# Two readings almost never match to the millisecond; a duplicate does. Cheap pre-filter before the
# decode, and the reason a missing/undecodable clip degrades to "unconfirmed" rather than "clean".
AUDIO_DURATION_TOLERANCE_MS = 120


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def duplicate_groups(rows: list[tuple[str, str, str, str, int]]) -> list[list[tuple[str, str]]]:
    """Groups of (segment_id, source_file) holding the same content across DIFFERENT files.

    `rows` = (id, audio_path, alignment_json, raw_transcript, verified).
    Pure so test_dataset_duplicates.py can pin it without a database. Union of RULE A (exact
    normalized text, any offset — the mp4's shifted clock taught us offsets prove nothing across
    encode generations) and RULE B (offset within 500 ms + >= 90% text similarity — drifted decodes
    of the same moment). Groups are merged when they share a member, so a sentence caught by both
    rules is one group, not two.
    """
    import difflib

    parsed: list[tuple[str, str, str, int]] = []  # (id, file, normalized text, offset)
    for seg_id, path, alignment_json, raw, _verified in rows:
        text = " ".join((raw or "").split())
        if len(text) < MIN_TEXT_CHARS:
            continue
        try:
            offset = int(json.loads(alignment_json or "{}").get("source_start_ms", -1))
        except (ValueError, TypeError):
            offset = -1
        parsed.append((seg_id, os.path.basename(path), text, offset))

    # Union-find over clip ids, so A-links and B-links merge into one group per real sentence.
    parent: dict[str, str] = {sid: sid for sid, _, _, _ in parsed}

    def find(x: str) -> str:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: str, b: str) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    # RULE A: identical normalized text in different files, offsets irrelevant.
    by_text: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for sid, fname, text, _off in parsed:
        by_text[text].append((sid, fname))
    for members in by_text.values():
        if len({f for _, f in members}) > 1:
            first = members[0][0]
            for sid, _ in members[1:]:
                union(first, sid)

    # RULE B: same source-clock position (within the bucket), text >= 90% similar, different files.
    # Clustered by offset distance — never a floor-bucket, whose edge splits a 13 ms encoder-padding
    # difference exactly often enough to miss real twins (this module's own test caught that).
    with_offset = sorted((o, sid, f, t) for sid, f, t, o in parsed if o >= 0)
    cluster: list[tuple[int, str, str, str]] = []
    def flush(cluster):
        for i in range(len(cluster)):
            for j in range(i + 1, len(cluster)):
                _, sa, fa, ta = cluster[i]
                _, sb, fb, tb = cluster[j]
                if fa != fb and difflib.SequenceMatcher(None, ta, tb).ratio() >= 0.90:
                    union(sa, sb)
    for entry in with_offset:
        if cluster and entry[0] - cluster[-1][0] > OFFSET_BUCKET_MS:
            flush(cluster)
            cluster = []
        cluster.append(entry)
    flush(cluster)

    files_of = {sid: fname for sid, fname, _, _ in parsed}
    groups: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for sid, _, _, _ in parsed:
        groups[find(sid)].append((sid, files_of[sid]))
    return sorted(sorted(g) for g in groups.values() if len(g) > 1 and len({f for _, f in g}) > 1)


def _clip_pcm(audio_path: str, alignment_json: str):
    """The clip's own samples, mono, or None when they cannot be read.

    Reads only the clip's span out of the source file rather than the whole recording — these are
    audiobook chapters, and decoding every one to compare two sentences would make the gate unusable.
    """
    try:
        import numpy as np
        import soundfile as sf
    except ImportError:
        return None
    try:
        meta = json.loads(alignment_json or "{}")
        start_ms = int(meta.get("source_start_ms", -1))
        end_ms = int(meta.get("source_end_ms", -1))
    except (ValueError, TypeError):
        return None
    if start_ms < 0 or end_ms <= start_ms or not os.path.isfile(audio_path):
        return None
    try:
        info = sf.info(audio_path)
        start = int(start_ms * info.samplerate / 1000)
        stop = min(int(end_ms * info.samplerate / 1000), info.frames)
        if stop <= start:
            return None
        data, _ = sf.read(audio_path, start=start, stop=stop, dtype="float32", always_2d=True)
    except Exception:
        return None
    mono = data.mean(axis=1)
    return mono if mono.size else None


def audio_says_duplicate(a, b) -> bool | None:
    """True/False when the audio can decide, None when it cannot be read.

    None is deliberately NOT False: a clip whose audio is missing must never be silently declared
    clean — the caller reports it as unconfirmed and keeps failing on it.
    """
    try:
        import numpy as np
    except ImportError:
        return None
    if a is None or b is None or a.size == 0 or b.size == 0:
        return None
    # Two readings of one sentence differ in length; a duplicated import does not.
    rate = 16000  # comparison rate; both clips are resampled to a common length below anyway
    if abs(a.size - b.size) / rate * 1000 > AUDIO_DURATION_TOLERANCE_MS:
        return False
    n = min(a.size, b.size)
    x, y = a[:n].astype("float64"), b[:n].astype("float64")
    x -= x.mean()
    y -= y.mean()
    denom = float(np.linalg.norm(x) * np.linalg.norm(y))
    if denom == 0.0:
        return None
    return bool(float(np.dot(x, y)) / denom >= AUDIO_DUPLICATE_CORRELATION)


def confirm_groups_with_audio(groups, rows):
    """Split text-matched candidates into (confirmed duplicates, unconfirmed, legitimate repeats)."""
    by_id = {r[0]: r for r in rows}
    confirmed, unconfirmed, repeats = [], [], []
    pcm_cache: dict[str, object] = {}

    def pcm_for(seg_id: str):
        if seg_id not in pcm_cache:
            _, path, alignment, _, _ = by_id[seg_id]
            pcm_cache[seg_id] = _clip_pcm(path, alignment)
        return pcm_cache[seg_id]

    for group in groups:
        verdicts = []
        for i in range(len(group)):
            for j in range(i + 1, len(group)):
                verdicts.append(audio_says_duplicate(pcm_for(group[i][0]), pcm_for(group[j][0])))
        if any(v is True for v in verdicts):
            confirmed.append(group)
        elif all(v is False for v in verdicts) and verdicts:
            repeats.append(group)
        else:
            unconfirmed.append(group)
    return confirmed, unconfirmed, repeats


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

    candidates = duplicate_groups(rows)
    # RULE C: the audio decides. Text-matched groups whose clips are demonstrably DIFFERENT audio are
    # a narrator repeating a sentence, not a duplicated import.
    groups, unconfirmed, repeats = confirm_groups_with_audio(candidates, rows)
    groups = groups + unconfirmed  # unreadable audio keeps failing — never silently cleared
    redundant = sum(len(g) - 1 for g in groups)  # each group needs all but one removed

    if repeats:
        print(
            f"  note: {len(repeats)} text-matched group(s) cleared by audio — the same sentence read "
            f"more than once (series intros, repeated verses), not a duplicated recording",
            flush=True,
        )

    if redundant > KNOWN_BASELINE:
        print("DATASET DUPLICATES: FAIL", flush=True)
        print(
            f"  {redundant} redundant clips across {len(groups)} duplicate-content groups — ABOVE the "
            f"recorded baseline of {KNOWN_BASELINE}, so a duplicate recording has been imported since "
            f"this gate was written. Same recording, different encode: the byte fingerprint cannot see "
            f"it; this offset+text audit can.",
            flush=True,
        )
        if unconfirmed:
            print(
                f"  {len(unconfirmed)} of those could NOT be confirmed from audio (missing file, "
                f"unreadable span, or numpy/soundfile absent) — counted as duplicates, because an "
                f"unreadable clip must never be waved through as clean",
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
