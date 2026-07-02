import csv
import json
import math
import os
import re
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path


# Defaults are neutral (no private local folder name in this public repo); point them at the real
# source/output with CORTEX_HALWEST_SOURCE_DIR / CORTEX_HALWEST_OUT_DIR.
SOURCE_DIR = Path(
    os.environ.get(
        "CORTEX_HALWEST_SOURCE_DIR",
        Path.home() / "cortex-audio" / "Halwest Voise Samles",
    )
)
OUT_DIR = Path(
    os.environ.get(
        "CORTEX_HALWEST_OUT_DIR",
        Path.home() / "cortex-audio" / "Halwest_Voice_Dataset",
    )
)
SPEAKER_ID = "halwest"

SILENCE_DB = "-35dB"
SILENCE_MIN_DUR = 0.35
MIN_SEG_DUR = 3.0
MAX_SEG_DUR = 12.0
TARGET_SEG_DUR = 7.0
ALLOW_INFERRED_TRANSCRIPT_PAIRS = os.environ.get("CORTEX_ALLOW_HALWEST_INFERRED_PAIRS") == "1"


@dataclass
class SourceItem:
    audio: Path
    transcript: Path | None
    transcript_status: str


def run(cmd: list[str], check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, text=True, capture_output=True, check=check)


def resolve_for_safety(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def validate_output_paths(source_dir: Path | None = None, out_dir: Path | None = None) -> tuple[Path, Path]:
    source = resolve_for_safety(source_dir or SOURCE_DIR)
    output = resolve_for_safety(out_dir or OUT_DIR)
    if output == source:
        raise ValueError(f"Output directory must not equal source directory: {output}")
    if output.anchor == str(output):
        raise ValueError(f"Refusing to use filesystem root as output directory: {output}")
    if source in output.parents:
        raise ValueError(f"Output directory must not be inside source directory: {output}")
    if output in source.parents:
        raise ValueError(f"Output directory must not be a parent of source directory: {output}")
    if output.name in {"", ".", ".."}:
        raise ValueError(f"Unsafe output directory name: {output}")
    return source, output


def remove_tree_checked(path: Path, allowed_parent: Path) -> None:
    target = resolve_for_safety(path)
    parent = resolve_for_safety(allowed_parent)
    if target == parent or parent not in target.parents:
        raise ValueError(f"Refusing to remove path outside expected parent: {target}")
    if target.exists():
        shutil.rmtree(target)


def prepare_output_dir() -> None:
    _, output = validate_output_paths()
    output_parent = output.parent
    preserved_asr_dir = None
    asr_dir = output / "asr_draft"
    if asr_dir.exists():
        preserved_asr_dir = output_parent / f"{output.name}_asr_draft_preserve"
        remove_tree_checked(preserved_asr_dir, output_parent)
        shutil.move(str(asr_dir), str(preserved_asr_dir))
    remove_tree_checked(output, output_parent)
    output.mkdir(parents=True, exist_ok=True)
    if preserved_asr_dir and preserved_asr_dir.exists():
        shutil.move(str(preserved_asr_dir), str(output / "asr_draft"))


def ffprobe_duration(path: Path) -> float:
    result = run(
        [
            "ffprobe",
            "-hide_banner",
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            str(path),
        ]
    )
    return float(result.stdout.strip())


def ffprobe_stream(path: Path) -> dict:
    result = run(
        [
            "ffprobe",
            "-hide_banner",
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,bits_per_sample",
            "-of",
            "json",
            str(path),
        ]
    )
    return json.loads(result.stdout)["streams"][0]


def read_text(path: Path | None) -> str:
    if not path or not path.exists() or path.stat().st_size == 0:
        return ""
    return path.read_text(encoding="utf-8-sig").strip()


def normalize_text(text: str) -> str:
    text = re.sub(r"\s+", " ", text.strip())
    text = text.replace(" ,", ",").replace(" .", ".")
    return text


def split_text_units(text: str) -> list[str]:
    text = normalize_text(text)
    if not text:
        return []

    # Arabic/Kurdish punctuation plus Latin punctuation. Kept ASCII via escapes.
    punct = r"([\u061f\u060c.!?;:]+)"
    pieces = re.split(punct, text)
    units: list[str] = []
    for idx in range(0, len(pieces), 2):
        body = pieces[idx].strip()
        mark = pieces[idx + 1].strip() if idx + 1 < len(pieces) else ""
        unit = normalize_text(f"{body}{mark}")
        if unit:
            units.append(unit)

    # Very long sentences are poor TTS targets. Split them on commas and merge
    # back into readable phrase groups.
    phrase_units: list[str] = []
    comma_re = re.compile(r"(?<=[\u060c,])\s+")
    for unit in units:
        if len(unit) <= 220:
            phrase_units.append(unit)
            continue
        parts = [p.strip() for p in comma_re.split(unit) if p.strip()]
        current = ""
        for part in parts:
            candidate = normalize_text(f"{current} {part}") if current else part
            if len(candidate) <= 180:
                current = candidate
            else:
                if current:
                    phrase_units.append(current)
                current = part
        if current:
            phrase_units.append(current)

    return phrase_units


def detect_silences(path: Path) -> list[tuple[float, float]]:
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-nostats",
        "-i",
        str(path),
        "-af",
        f"silencedetect=noise={SILENCE_DB}:d={SILENCE_MIN_DUR}",
        "-f",
        "null",
        "-",
    ]
    result = run(cmd, check=False)
    text = result.stderr
    starts = [float(m.group(1)) for m in re.finditer(r"silence_start: ([0-9.]+)", text)]
    ends = [float(m.group(1)) for m in re.finditer(r"silence_end: ([0-9.]+)", text)]
    silences: list[tuple[float, float]] = []
    for start, end in zip(starts, ends):
        if end > start:
            silences.append((start, end))
    return silences


def build_speech_intervals(duration: float, silences: list[tuple[float, float]]) -> list[tuple[float, float]]:
    intervals: list[tuple[float, float]] = []
    cursor = 0.0
    for start, end in silences:
        if start > cursor:
            intervals.append((cursor, start))
        cursor = max(cursor, end)
    if cursor < duration:
        intervals.append((cursor, duration))

    cleaned: list[tuple[float, float]] = []
    for start, end in intervals:
        start = max(0.0, start - 0.03)
        end = min(duration, end + 0.05)
        if end - start >= 0.25:
            cleaned.append((start, end))
    return cleaned


def merge_intervals(intervals: list[tuple[float, float]]) -> list[tuple[float, float]]:
    chunks: list[tuple[float, float]] = []
    cur_start: float | None = None
    cur_end: float | None = None

    for start, end in intervals:
        if cur_start is None:
            cur_start, cur_end = start, end
            continue
        assert cur_end is not None
        candidate = end - cur_start
        current = cur_end - cur_start
        if candidate <= MAX_SEG_DUR and (current < MIN_SEG_DUR or candidate <= TARGET_SEG_DUR):
            cur_end = end
        else:
            if cur_end - cur_start >= MIN_SEG_DUR:
                chunks.append((cur_start, cur_end))
            cur_start, cur_end = start, end

    if cur_start is not None and cur_end is not None and cur_end - cur_start >= MIN_SEG_DUR:
        chunks.append((cur_start, cur_end))

    # Second pass: merge any trailing short chunk into previous if possible.
    merged: list[tuple[float, float]] = []
    for start, end in chunks:
        if merged and end - start < MIN_SEG_DUR:
            prev_start, _ = merged[-1]
            if end - prev_start <= MAX_SEG_DUR:
                merged[-1] = (prev_start, end)
                continue
        merged.append((start, end))
    return merged


def allocate_text(chunks: list[tuple[float, float]], units: list[str]) -> list[str]:
    if not chunks or not units:
        return []

    total_dur = sum(end - start for start, end in chunks)
    total_chars = sum(len(u) for u in units)
    texts: list[str] = []
    unit_idx = 0

    for chunk_idx, (start, end) in enumerate(chunks):
        remaining_chunks = len(chunks) - chunk_idx
        remaining_units = len(units) - unit_idx
        if remaining_units <= 0:
            texts.append("")
            continue
        if remaining_chunks == 1:
            texts.append(normalize_text(" ".join(units[unit_idx:])))
            unit_idx = len(units)
            continue

        target_chars = total_chars * ((end - start) / total_dur)
        selected: list[str] = []
        selected_chars = 0
        while unit_idx < len(units):
            unit = units[unit_idx]
            if selected and selected_chars >= target_chars * 0.85:
                break
            if remaining_units <= remaining_chunks:
                break
            selected.append(unit)
            selected_chars += len(unit)
            unit_idx += 1
            remaining_units -= 1

        if not selected and unit_idx < len(units):
            selected.append(units[unit_idx])
            unit_idx += 1
        texts.append(normalize_text(" ".join(selected)))

    return texts


def quality_status(duration: float, text: str) -> tuple[str, list[str]]:
    issues: list[str] = []
    if duration < MIN_SEG_DUR:
        issues.append("short_duration")
    if duration > MAX_SEG_DUR:
        issues.append("long_duration")
    if not text:
        issues.append("missing_text")
    cps = len(text) / duration if duration > 0 else 0
    if text and (cps < 6 or cps > 28):
        issues.append("char_rate_outlier")
    status = "train" if not issues else "review"
    return status, issues


def training_trusted(item: SourceItem) -> bool:
    return item.transcript_status == "matched_by_basename"


def convert_full_audio(source: Path, dest_48k: Path, dest_24k: Path) -> None:
    dest_48k.parent.mkdir(parents=True, exist_ok=True)
    dest_24k.parent.mkdir(parents=True, exist_ok=True)
    run(
        [
            "ffmpeg",
            "-hide_banner",
            "-y",
            "-i",
            str(source),
            "-ac",
            "1",
            "-ar",
            "48000",
            "-sample_fmt",
            "s16",
            "-af",
            "highpass=f=60,lowpass=f=18000,loudnorm=I=-20:TP=-2:LRA=11",
            str(dest_48k),
        ]
    )
    run(
        [
            "ffmpeg",
            "-hide_banner",
            "-y",
            "-i",
            str(source),
            "-ac",
            "1",
            "-ar",
            "24000",
            "-sample_fmt",
            "s16",
            "-af",
            "highpass=f=60,lowpass=f=11500,loudnorm=I=-20:TP=-2:LRA=11",
            str(dest_24k),
        ]
    )


def extract_segment(source: Path, start: float, end: float, dest: Path, sample_rate: int = 24000) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    lowpass = 11500 if sample_rate <= 24000 else 18000
    run(
        [
            "ffmpeg",
            "-hide_banner",
            "-y",
            "-ss",
            f"{start:.3f}",
            "-to",
            f"{end:.3f}",
            "-i",
            str(source),
            "-ac",
            "1",
            "-ar",
            str(sample_rate),
            "-sample_fmt",
            "s16",
            "-af",
            f"highpass=f=60,lowpass=f={lowpass},loudnorm=I=-20:TP=-2:LRA=11",
            str(dest),
        ]
    )


def pair_sources() -> list[SourceItem]:
    wavs = sorted(SOURCE_DIR.glob("*.wav"))
    txts = {p.stem.replace(".txt", ""): p for p in SOURCE_DIR.glob("*.txt*")}
    items: list[SourceItem] = []
    for wav in wavs:
        exact = txts.get(wav.stem)
        if exact and exact.stat().st_size > 0:
            items.append(SourceItem(wav, exact, "matched_by_basename"))
            continue
        if ALLOW_INFERRED_TRANSCRIPT_PAIRS and wav.stem == "B7890RX":
            inferred = SOURCE_DIR / "B8790RX.txt.txt"
            if inferred.exists() and inferred.stat().st_size > 0:
                items.append(SourceItem(wav, inferred, "inferred_B8790RX_transcript_explicitly_allowed_review_only"))
                continue
        if exact and exact.stat().st_size == 0:
            items.append(SourceItem(wav, exact, "transcript_file_empty"))
        else:
            items.append(SourceItem(wav, None, "missing_transcript"))
    return items


def write_jsonl(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


def write_csv(path: Path, fieldnames: list[str], rows: list[dict], delimiter: str = ",") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames, delimiter=delimiter, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def copy_source_assets(items: list[SourceItem]) -> None:
    dest = OUT_DIR / "source_assets"
    dest.mkdir(parents=True, exist_ok=True)
    for item in items:
        shutil.copy2(item.audio, dest / item.audio.name)
        if item.transcript:
            shutil.copy2(item.transcript, dest / item.transcript.name)


def make_readme(summary: dict, missing_rows: list[dict], has_asr_draft: bool = False) -> str:
    missing_note = "\n".join(
        f"- {row['audio_file']}: {row['transcript_status']}" for row in missing_rows
    )
    if not missing_note:
        missing_note = "- None"
    asr_note = ""
    if has_asr_draft:
        asr_note = """
## ASR Drafts

- `asr_draft/B7876RX/`: draft ASR transcript for the untranscribed `B7876RX.wav`.

These rows are marked as draft and must be human-corrected before they are promoted into TTS or voice-cloning training labels.
"""
    return f"""# Halwest Voice Dataset

Speaker: `{SPEAKER_ID}`

This dataset was generated from the WAV files in:
`{SOURCE_DIR}`

## Outputs

- `general_ai_training/`: full-file normalized audio plus file-level transcripts.
- `tts_voice_cloning/`: silence-based clips with transcript text, exported in LJSpeech-style `metadata.csv` and JSONL.
- `voice_cloning_reference/`: best clean reference clips selected from the trainable TTS clips.
- `review/`: pairing, missing-transcript, and per-segment review files.
- `source_assets/`: copied originals for traceability.

## Important Accuracy Notes

The transcript-backed sets preserve the provided transcript order and split it across silence-detected audio chunks. This is suitable for a first professional dataset pass, but every row marked `review` in `review/segment_review.csv` should be listened to before final model training. Forced alignment or manual listening is still required for true "perfect" clip-level text timing.

**This applies to `train`-status rows too.** Their clip-level transcript is allocated to each chunk by proportional char-count (in document order), NOT by forced alignment — see each row's `alignment_method` field (e.g. `silence_split_transcript_order`). So a `train` clip whose silence boundary falls mid-sentence can carry text shifted from what is actually spoken, even though it passed the duration/char-rate quality check. Before high-stakes training, run a forced aligner over the `train` split (or spot-listen) and drop/refit any row whose `alignment_method` is not a verified alignment.

Only exact same-basename transcript pairs are trusted for TTS/voice-cloning training. Inferred transcript candidates are disabled by default and, even when explicitly enabled with `CORTEX_ALLOW_HALWEST_INFERRED_PAIRS=1`, are exported as review-only material until a human confirms the pairing and timing.

## Summary

- Source audio files: {summary['source_audio_files']}
- Transcript-backed source files: {summary['transcript_backed_files']}
- Full normalized files: {summary['full_normalized_files']}
- TTS train clips: {summary['tts_train_clips']}
- TTS review clips: {summary['tts_review_clips']}
- Voice-cloning reference clips: {summary['reference_clips']}

## Missing Or Empty Transcripts

{missing_note}
{asr_note}
"""


def main() -> None:
    if not SOURCE_DIR.exists():
        raise FileNotFoundError(SOURCE_DIR)
    prepare_output_dir()

    items = pair_sources()
    copy_source_assets(items)

    source_report: list[dict] = []
    missing_rows: list[dict] = []
    general_rows: list[dict] = []
    general_jsonl: list[dict] = []
    tts_rows: list[dict] = []
    tts_jsonl: list[dict] = []
    review_rows: list[dict] = []
    train_candidates: list[dict] = []

    for item in items:
        duration = ffprobe_duration(item.audio)
        stream = ffprobe_stream(item.audio)
        transcript_raw = read_text(item.transcript)
        transcript = normalize_text(transcript_raw)
        units = split_text_units(transcript)
        silences = detect_silences(item.audio)
        intervals = build_speech_intervals(duration, silences)
        chunks = merge_intervals(intervals)

        source_report.append(
            {
                "audio_file": item.audio.name,
                "duration_sec": round(duration, 3),
                "codec": stream.get("codec_name"),
                "sample_rate": stream.get("sample_rate"),
                "channels": stream.get("channels"),
                "bits_per_sample": stream.get("bits_per_sample"),
                "transcript_file": item.transcript.name if item.transcript else "",
                "transcript_status": item.transcript_status,
                "transcript_chars": len(transcript),
                "text_units": len(units),
                "silences_detected": len(silences),
                "speech_chunks": len(chunks),
            }
        )

        if not transcript:
            missing_rows.append(
                {
                    "audio_file": item.audio.name,
                    "transcript_file": item.transcript.name if item.transcript else "",
                    "transcript_status": item.transcript_status,
                    "duration_sec": round(duration, 3),
                }
            )

        full_48 = OUT_DIR / "general_ai_training" / "wavs_48k_full" / item.audio.name
        full_24 = OUT_DIR / "general_ai_training" / "wavs_24k_full" / item.audio.name
        convert_full_audio(item.audio, full_48, full_24)

        general_id = item.audio.stem
        general_rows.append(
            {
                "id": general_id,
                "audio_48k": str(full_48),
                "audio_24k": str(full_24),
                "source_audio": str(item.audio),
                "transcript_file": str(item.transcript) if item.transcript else "",
                "transcript_status": item.transcript_status,
                "duration_sec": round(duration, 3),
                "text": transcript,
            }
        )
        general_jsonl.append(
            {
                "id": general_id,
                "speaker": SPEAKER_ID,
                "audio_filepath": str(full_24),
                "source_audio": str(item.audio),
                "duration": round(duration, 3),
                "text": transcript,
                "transcript_status": item.transcript_status,
            }
        )

        if not transcript:
            continue

        allocated = allocate_text(chunks, units)
        for idx, ((start, end), text) in enumerate(zip(chunks, allocated), start=1):
            seg_id = f"{item.audio.stem}_{idx:04d}"
            seg_dur = end - start
            status, issues = quality_status(seg_dur, text)
            if not training_trusted(item):
                status = "review"
                issues.append("untrusted_transcript_pair")
            if status == "train":
                wav_path = OUT_DIR / "tts_voice_cloning" / "wavs" / f"{seg_id}.wav"
            else:
                wav_path = OUT_DIR / "review" / "review_wavs" / f"{seg_id}.wav"
            extract_segment(item.audio, start, end, wav_path, sample_rate=24000)
            row = {
                "id": seg_id,
                "speaker": SPEAKER_ID,
                "audio_file": str(wav_path),
                "source_audio": str(item.audio),
                "source_transcript": str(item.transcript) if item.transcript else "",
                "start_sec": round(start, 3),
                "end_sec": round(end, 3),
                "duration_sec": round(seg_dur, 3),
                "text": text,
                "text_chars": len(text),
                "char_per_sec": round(len(text) / seg_dur, 3) if seg_dur > 0 else 0,
                "status": status,
                "issues": ";".join(issues),
                "alignment_method": "silence_split_transcript_order",
            }
            review_rows.append(row)
            if status == "train":
                tts_rows.append({"id": seg_id, "text": text, "normalized_text": text})
                tts_jsonl.append(
                    {
                        "id": seg_id,
                        "speaker": SPEAKER_ID,
                        "audio_filepath": str(wav_path),
                        "source_audio": str(item.audio),
                        "offset": round(start, 3),
                        "duration": round(seg_dur, 3),
                        "text": text,
                    }
                )
                train_candidates.append(row)

    write_csv(
        OUT_DIR / "review" / "source_pairing_report.csv",
        [
            "audio_file",
            "duration_sec",
            "codec",
            "sample_rate",
            "channels",
            "bits_per_sample",
            "transcript_file",
            "transcript_status",
            "transcript_chars",
            "text_units",
            "silences_detected",
            "speech_chunks",
        ],
        source_report,
    )
    write_csv(
        OUT_DIR / "review" / "missing_transcripts.csv",
        ["audio_file", "transcript_file", "transcript_status", "duration_sec"],
        missing_rows,
    )
    write_csv(
        OUT_DIR / "review" / "segment_review.csv",
        [
            "id",
            "speaker",
            "audio_file",
            "source_audio",
            "source_transcript",
            "start_sec",
            "end_sec",
            "duration_sec",
            "text",
            "text_chars",
            "char_per_sec",
            "status",
            "issues",
            "alignment_method",
        ],
        review_rows,
    )
    write_csv(
        OUT_DIR / "general_ai_training" / "metadata.csv",
        [
            "id",
            "audio_48k",
            "audio_24k",
            "source_audio",
            "transcript_file",
            "transcript_status",
            "duration_sec",
            "text",
        ],
        general_rows,
    )
    write_jsonl(OUT_DIR / "general_ai_training" / "manifest.jsonl", general_jsonl)

    metadata_path = OUT_DIR / "tts_voice_cloning" / "metadata.csv"
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    with metadata_path.open("w", encoding="utf-8", newline="\n") as f:
        for row in tts_rows:
            f.write(f"{row['id']}|{row['text']}|{row['normalized_text']}\n")
    write_jsonl(OUT_DIR / "tts_voice_cloning" / "manifest.jsonl", tts_jsonl)

    # Pick reference clips that are long enough for cloning prompts, text-backed,
    # and not suspiciously fast or slow by text density.
    ref_candidates = [
        r
        for r in train_candidates
        if 6.0 <= float(r["duration_sec"]) <= 12.0 and 8.0 <= float(r["char_per_sec"]) <= 24.0
    ]
    ref_candidates.sort(key=lambda r: (abs(float(r["duration_sec"]) - 9.0), -int(r["text_chars"])))
    reference_rows: list[dict] = []
    for idx, row in enumerate(ref_candidates[:20], start=1):
        src = Path(row["audio_file"])
        dest = OUT_DIR / "voice_cloning_reference" / "top_reference_clips" / f"ref_{idx:02d}_{src.name}"
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        ref_row = dict(row)
        ref_row["reference_rank"] = idx
        ref_row["reference_audio_file"] = str(dest)
        reference_rows.append(ref_row)

    write_csv(
        OUT_DIR / "voice_cloning_reference" / "reference_manifest.csv",
        [
            "reference_rank",
            "id",
            "speaker",
            "reference_audio_file",
            "source_audio",
            "start_sec",
            "end_sec",
            "duration_sec",
            "text",
            "text_chars",
            "char_per_sec",
            "status",
            "issues",
            "alignment_method",
        ],
        reference_rows,
    )

    summary = {
        "source_audio_files": len(items),
        "transcript_backed_files": sum(1 for i in items if read_text(i.transcript)),
        "full_normalized_files": len(general_rows) * 2,
        "tts_train_clips": len(tts_rows),
        "tts_review_clips": sum(1 for r in review_rows if r["status"] == "review"),
        "reference_clips": len(reference_rows),
    }
    (OUT_DIR / "README.md").write_text(
        make_readme(summary, missing_rows, has_asr_draft=(OUT_DIR / "asr_draft").exists()),
        encoding="utf-8",
    )
    (OUT_DIR / "dataset_manifest.json").write_text(
        json.dumps(
            {
                "speaker": SPEAKER_ID,
                "source_dir": str(SOURCE_DIR),
                "output_dir": str(OUT_DIR),
                "settings": {
                    "silence_db": SILENCE_DB,
                    "silence_min_duration": SILENCE_MIN_DUR,
                    "min_segment_duration": MIN_SEG_DUR,
                    "max_segment_duration": MAX_SEG_DUR,
                    "target_segment_duration": TARGET_SEG_DUR,
                },
                "summary": summary,
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )

    print(json.dumps(summary, ensure_ascii=False, indent=2))
    print(str(OUT_DIR))


if __name__ == "__main__":
    main()
