#!/usr/bin/env python3
"""Non-destructive TTS triage, NEVER a gold certificate.

100ms frame metrics follow audio_quality.rs for mono PCM16. The field named snr_db
is an amplitude-range proxy, not a calibrated speech/noise or overlap detector.
Similarity metadata is advisory and cannot prove target voice or clean boundaries.
Output verdicts are hold or pending_qualification, never keep/gold. Every WAV gets
a row, including unreadable input. Missing/ambiguous evidence stays visible.

Exit 0 means the audit completed, NOT that clips qualify for training; exit 2
means input/metadata could not be fully measured. No originals are moved/deleted.
"""

from __future__ import annotations

import argparse
import csv
import math
import hashlib
import statistics
import sys
import wave
from array import array
from pathlib import Path

# Existing thresholds are triage only; no recalibration or gold admission is implied.
POOR_AUDIO_SNR_DB = 5.0
POOR_AUDIO_CLIPPING_RATIO = 0.1
FRAME_MS = 100
CLIP_RAIL = 32760


def measure(path: Path) -> dict | None:
    """Per-clip metrics, or None if the file cannot be read as PCM WAV."""
    try:
        before = path.stat()
        with wave.open(str(path), "rb") as w:
            if w.getsampwidth() != 2 or w.getnchannels() != 1 or w.getcomptype() != "NONE":
                return None
            sr, channels = w.getframerate(), w.getnchannels()
            frames = w.getnframes()
            raw = w.readframes(frames)
            if len(raw) != frames * channels * 2:
                return None
        after = path.stat()
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            return None
    except (OSError, ValueError, EOFError, wave.Error):
        return None

    pcm = array("h")
    pcm.frombytes(raw)
    if sys.byteorder != "little":
        pcm.byteswap()
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
        "snr_db": snr_db,
        "clipped_samples": clipped,
        "clipping_ratio": clipped / n,
        "constant_signal": min(pcm) == max(pcm),
        "pcm_sha256": hashlib.sha256(raw).hexdigest(),
        "rms_db": round(rms_db, 2),
        "lead_silence_s": round(lead, 2),
        "tail_silence_s": round(tail, 2),
        "dc_offset": dc_offset,
    }


def load_similarity(folder: Path) -> dict[str, float]:
    """Advisory scores only. Duplicate names (even identical rows) fail closed."""
    meta = folder.parent / "metadata.csv"
    if not meta.is_file():
        return {}
    out: dict[str, float] = {}
    seen: set[str] = set()
    with meta.open(encoding="utf-8-sig", newline="") as f:
        reader = csv.DictReader(f, delimiter="|")
        if not reader.fieldnames or "audio_path" not in reader.fieldnames:
            raise ValueError("metadata_missing_audio_path_column")
        for row in reader:
            relative = Path((row.get("audio_path") or "").replace("\\", "/"))
            resolved = (meta.parent / relative).resolve()
            if relative.is_absolute() or resolved.parent != folder.resolve():
                raise ValueError("metadata_audio_path_outside_requested_folder")
            name = relative.name.casefold()
            if name in seen:
                raise ValueError(f"duplicate_metadata_audio_path:{relative.name}")
            seen.add(name)
            if not resolved.is_file():
                raise ValueError(f"metadata_audio_missing:{relative.name}")
            try:
                score = float(row.get("similarity_score") or row.get("speaker_similarity"))
            except (TypeError, ValueError):
                continue
            if math.isfinite(score) and -1 <= score <= 1:
                out[name] = score
    return out


def classify(m: dict, args: argparse.Namespace, metadata_error: str | None = None) -> tuple[str, list[str]]:
    reasons = []
    if m.get("audio_error"):
        reasons.append(m["audio_error"])
    else:
        if m["constant_signal"]:
            reasons.append("constant_or_silent_audio")
        if m["snr_db"] is None:
            reasons.append("snr_unmeasurable")
        elif m["snr_db"] < args.min_snr:
            reasons.append("low_dynamic_range_proxy")
        if m["clipping_ratio"] > args.max_clipping:
            reasons.append("clipping_candidate")
        if m["tail_silence_s"] > args.max_tail_silence:
            reasons.append("long_tail_candidate")
        if abs(m["dc_offset"]) > 0.01:
            reasons.append("dc_offset_candidate")
    if metadata_error:
        reasons.append(metadata_error)
    if m.get("similarity") is None:
        reasons.append("speaker_similarity_missing_or_invalid")
    elif m["similarity"] < args.min_similarity:
        reasons.append("speaker_similarity_candidate")
    # A numerically clean clip still lacks explicit overlap/identity/text qualification.
    return ("hold" if reasons else "pending_qualification"), reasons


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("folder", type=Path)
    ap.add_argument("--out", type=Path, default=Path("tts_clip_quality.csv"))
    ap.add_argument("--min-snr", type=float, default=POOR_AUDIO_SNR_DB)
    ap.add_argument("--max-clipping", type=float, default=0.0, help="TTS default: ANY clipped sample is suspect")
    ap.add_argument("--min-similarity", type=float, default=0.65)
    ap.add_argument("--max-tail-silence", type=float, default=1.0)
    args = ap.parse_args()
    if not all(math.isfinite(v) for v in (args.min_snr, args.max_clipping, args.min_similarity, args.max_tail_silence)):
        ap.error("thresholds must be finite")
    if not 0 <= args.max_clipping <= 1 or not -1 <= args.min_similarity <= 1 or args.max_tail_silence < 0:
        ap.error("invalid threshold range")

    files = sorted(p for p in args.folder.glob("*.wav"))
    if not files:
        print(f"no .wav files in {args.folder}")
        return 1
    metadata_error = None
    try:
        similarity = load_similarity(args.folder)
    except (OSError, ValueError, csv.Error) as error:
        similarity = {}
        metadata_error = f"metadata_integrity_error:{error}"
    print(f"measuring {len(files)} clip(s) in {args.folder}", flush=True)

    rows, unreadable = [], []
    for i, f in enumerate(files, 1):
        m = measure(f)
        if m is None:
            unreadable.append(f.name)
            m = {"file": f.name, "audio_error": "unreadable_unsupported_or_changed_audio"}
        m["similarity"] = similarity.get(f.name.casefold())
        rows.append(m)
        if i % 2000 == 0:
            print(f"  {i}/{len(files)}", flush=True)

    reasons: dict[str, list[str]] = {}
    for m in rows:
        m["verdict"], why = classify(m, args, metadata_error)
        m["reasons"] = "; ".join(why)
        m["tts_gold_certified"] = False
        if why:
            reasons[m["file"]] = why

    flagged = args.out.with_name(args.out.stem + "_flagged.txt")
    if args.out.exists() or flagged.exists():
        print("Refusing to replace existing audit evidence", file=sys.stderr)
        return 2
    with args.out.open("x", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=sorted({key for row in rows for key in row}))
        w.writeheader()
        w.writerows(rows)
    with flagged.open("x", encoding="utf-8") as target:
        target.write("\n".join(sorted(reasons)))

    snrs = [m["snr_db"] for m in rows if m.get("snr_db") is not None]
    print()
    print(f"measured        : {len(rows)-len(unreadable)}   unreadable: {len(unreadable)}")
    if snrs:
        snrs.sort()
        print(f"SNR dB          : min {snrs[0]:.1f} | p10 {snrs[len(snrs)//10]:.1f} | median "
              f"{statistics.median(snrs):.1f} | max {snrs[-1]:.1f}")
    rmss = sorted(m["rms_db"] for m in rows if "rms_db" in m)
    if rmss:
        print(f"RMS dBFS        : median {statistics.median(rmss):.1f}")
    print(f"any clipping    : {sum(1 for m in rows if m.get('clipped_samples', 0) > 0)}")
    print(f"FLAGGED         : {len(reasons)}  ({len(reasons)/len(rows)*100:.1f}%)")
    tally: dict[str, int] = {}
    for why in reasons.values():
        tally[why[0].split()[0]] = tally.get(why[0].split()[0], 0) + 1
    for k, v in sorted(tally.items(), key=lambda kv: -kv[1]):
        print(f"   {k:12s} {v}")
    print(f"\nfull report : {args.out}")
    print(f"flagged list: {flagged}   (nothing was moved or deleted)")
    print("TTS GOLD CERTIFIED: 0. No warning is not proof of clean speech, correct voice, or complete text.")
    return 2 if unreadable or metadata_error else 0


if __name__ == "__main__":
    sys.exit(main())
