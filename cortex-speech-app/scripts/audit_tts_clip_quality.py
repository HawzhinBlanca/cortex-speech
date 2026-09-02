#!/usr/bin/env python3
"""Measure every clip in a prepared TTS folder, so a "keep only the gold" cull is decided by numbers.

Auditioning tens of thousands of clips by ear is not a plan — it is slow, it drifts as the ear tires,
and the clips that hurt a TTS model most (a truncated onset, a clipped rail, a noise floor 6 dB under
the voice) are the ones a quick listen forgives. This measures all of them and ranks the damage.

Mirrors `audio_quality.rs` EXACTLY — same 100 ms frames, same lowest/highest-10% noise and signal
floors, same 32760 clipping rail, same "fewer than 3 frames means SNR is UNMEASURABLE, not zero".
A separate estimator would produce a second opinion that disagrees with the app's own gates on the
same file, and then neither number could be trusted. Thresholds come from `quality.rs`.

What it reports per clip, worst first:
  * snr_db          — voice against its own noise floor. The app calls < 5 dB poor.
  * clipping_ratio  — samples at the 16-bit rail. The app calls > 0.1 poor; for TTS, ANY is suspect.
  * rms_db          — level. A set normalized to a loudness target should be tight; outliers are
                      clips whose speech is mostly silence, or that are pinned to the ceiling.
  * lead/tail_sil   — leading and trailing silence. Long tails teach a TTS model to trail off.
  * dc_offset       — a DC bias no listener hears and every vocoder does.
  * similarity      — the speaker-verification score already in metadata.csv, joined by filename.

It writes a CSV of every clip and a plain list of the ones that fail, and it MOVES NOTHING. Deciding
what leaves the dataset is the owner's call; this exists to make that call an informed one.

Run:  python scripts/audit_tts_clip_quality.py <folder> [--out report.csv] [--min-snr 5]
                                               [--max-clipping 0.0] [--min-similarity 0.65]
"""

from __future__ import annotations

import argparse
import csv
import math
import statistics
import sys
import wave
from array import array
from pathlib import Path

# Mirrors quality.rs. Kept as the defaults so a clip this audit passes is a clip the app's own gates
# also pass — a stricter number here would reject work the pipeline would happily have used.
POOR_AUDIO_SNR_DB = 5.0
POOR_AUDIO_CLIPPING_RATIO = 0.1
FRAME_MS = 100
CLIP_RAIL = 32760


def measure(path: Path) -> dict | None:
    """Per-clip metrics, or None if the file cannot be read as PCM WAV."""
    try:
        with wave.open(str(path), "rb") as w:
            if w.getsampwidth() != 2:
                return None
            sr, channels = w.getframerate(), w.getnchannels()
            raw = w.readframes(w.getnframes())
    except Exception:
        return None

    pcm = array("h")
    pcm.frombytes(raw[: len(raw) // 2 * 2])
    if channels > 1:  # mix to mono the cheap way; these sets are mono in practice
        pcm = array("h", [int(sum(pcm[i : i + channels]) / channels) for i in range(0, len(pcm) - channels, channels)])
    n = len(pcm)
    if n == 0:
        return None

    sum_sq = 0.0
    clipped = 0
    total = 0
    for s in pcm:
        v = s / 32768.0
        sum_sq += v * v
        total += s
        if abs(s) >= CLIP_RAIL:
            clipped += 1
    rms = math.sqrt(sum_sq / n)
    rms_db = 20.0 * math.log10(rms) if rms > 1e-10 else -100.0
    dc_offset = (total / n) / 32768.0

    frame = max(1, int(sr * FRAME_MS / 1000))
    frame_rms = []
    for i in range(0, n, frame):
        chunk = pcm[i : i + frame]
        if len(chunk) < frame // 2:
            continue
        acc = 0.0
        for s in chunk:
            v = s / 32768.0
            acc += v * v
        frame_rms.append(math.sqrt(acc / len(chunk)))

    snr_db = None
    if len(frame_rms) >= 3:
        ordered = sorted(frame_rms)
        k = max(1, len(ordered) // 10)
        noise = sum(ordered[:k]) / k
        signal = sum(ordered[-k:]) / k
        # Same guard as the Rust: equal floors mean SNR is UNDEFINED, never 0 dB (which the gates
        # would read as the worst class and reject a legitimate clip on).
        if noise > 1e-10 and signal > noise * (1.0 + 1e-6):
            snr_db = 20.0 * math.log10(signal / noise)

    # Silence measured against this clip's own noise floor, not an absolute — a quiet recording is not
    # a silent one. A frame within 3 dB of the noise floor counts as silence.
    lead = tail = 0.0
    if frame_rms:
        floor = min(frame_rms) * math.sqrt(2)
        i = 0
        while i < len(frame_rms) and frame_rms[i] <= floor:
            i += 1
        lead = i * FRAME_MS / 1000.0
        j = len(frame_rms) - 1
        while j >= 0 and frame_rms[j] <= floor:
            j -= 1
        tail = (len(frame_rms) - 1 - j) * FRAME_MS / 1000.0

    return {
        "file": path.name,
        "duration_s": round(n / sr, 3),
        "sample_rate": sr,
        "snr_db": None if snr_db is None else round(snr_db, 2),
        "clipping_ratio": round(clipped / n, 6),
        "rms_db": round(rms_db, 2),
        "lead_silence_s": round(lead, 2),
        "tail_silence_s": round(tail, 2),
        "dc_offset": round(dc_offset, 5),
    }


def load_similarity(folder: Path) -> dict[str, float]:
    """Speaker-verification scores the dataset already computed, joined by filename."""
    meta = folder.parent / "metadata.csv"
    if not meta.is_file():
        return {}
    out: dict[str, float] = {}
    try:
        with meta.open(encoding="utf-8") as f:
            for row in csv.DictReader(f, delimiter="|"):
                name = Path((row.get("audio_path") or "").replace("\\", "/")).name
                try:
                    out[name] = float(row.get("similarity_score") or row.get("speaker_similarity"))
                except (TypeError, ValueError):
                    continue
    except OSError:
        return {}
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("folder", type=Path)
    ap.add_argument("--out", type=Path, default=Path("tts_clip_quality.csv"))
    ap.add_argument("--min-snr", type=float, default=POOR_AUDIO_SNR_DB)
    ap.add_argument("--max-clipping", type=float, default=0.0, help="TTS default: ANY clipped sample is suspect")
    ap.add_argument("--min-similarity", type=float, default=0.65)
    ap.add_argument("--max-tail-silence", type=float, default=1.0)
    args = ap.parse_args()

    files = sorted(p for p in args.folder.glob("*.wav"))
    if not files:
        print(f"no .wav files in {args.folder}")
        return 1
    similarity = load_similarity(args.folder)
    print(f"measuring {len(files)} clip(s) in {args.folder}", flush=True)

    rows, unreadable = [], []
    for i, f in enumerate(files, 1):
        m = measure(f)
        if m is None:
            unreadable.append(f.name)
            continue
        m["similarity"] = similarity.get(f.name)
        rows.append(m)
        if i % 2000 == 0:
            print(f"  {i}/{len(files)}", flush=True)

    reasons: dict[str, list[str]] = {}
    for m in rows:
        why = []
        if m["snr_db"] is not None and m["snr_db"] < args.min_snr:
            why.append(f"snr {m['snr_db']}dB")
        if m["clipping_ratio"] > args.max_clipping:
            why.append(f"clipping {m['clipping_ratio']}")
        if m["similarity"] is not None and m["similarity"] < args.min_similarity:
            why.append(f"similarity {m['similarity']}")
        if m["tail_silence_s"] > args.max_tail_silence:
            why.append(f"tail silence {m['tail_silence_s']}s")
        if abs(m["dc_offset"]) > 0.01:
            why.append(f"dc offset {m['dc_offset']}")
        if why:
            reasons[m["file"]] = why

    with args.out.open("w", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0].keys()) + ["verdict"])
        w.writeheader()
        for m in sorted(rows, key=lambda r: (r["snr_db"] is None, r["snr_db"] if r["snr_db"] is not None else 999)):
            w.writerow({**m, "verdict": "; ".join(reasons.get(m["file"], [])) or "keep"})
    flagged = args.out.with_name(args.out.stem + "_flagged.txt")
    flagged.write_text("\n".join(sorted(reasons)), encoding="utf-8")

    snrs = [m["snr_db"] for m in rows if m["snr_db"] is not None]
    print()
    print(f"measured        : {len(rows)}   unreadable: {len(unreadable)}")
    if snrs:
        snrs.sort()
        print(f"SNR dB          : min {snrs[0]:.1f} | p10 {snrs[len(snrs)//10]:.1f} | median "
              f"{statistics.median(snrs):.1f} | max {snrs[-1]:.1f}")
    rmss = sorted(m["rms_db"] for m in rows)
    print(f"RMS dBFS        : p10 {rmss[len(rmss)//10]:.1f} | median {statistics.median(rmss):.1f} "
          f"| p90 {rmss[len(rmss)*9//10]:.1f}")
    print(f"any clipping    : {sum(1 for m in rows if m['clipping_ratio'] > 0)}")
    print(f"FLAGGED         : {len(reasons)}  ({len(reasons)/len(rows)*100:.1f}%)")
    tally: dict[str, int] = {}
    for why in reasons.values():
        tally[why[0].split()[0]] = tally.get(why[0].split()[0], 0) + 1
    for k, v in sorted(tally.items(), key=lambda kv: -kv[1]):
        print(f"   {k:12s} {v}")
    print(f"\nfull report : {args.out}")
    print(f"flagged list: {flagged}   (nothing was moved or deleted)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
