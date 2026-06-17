import csv
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path

from build_halwest_dataset import OUT_DIR, SPEAKER_ID, resolve_for_safety


def tts_dir() -> Path:
    return OUT_DIR / "tts_voice_cloning"


def splits_dir() -> Path:
    return tts_dir() / "splits"


def qc_dir() -> Path:
    return OUT_DIR / "quality_control"


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def checked_manifest_audio_path(row: dict, key: str = "audio_filepath", out_dir: Path | None = None) -> Path:
    raw_path = str(row.get(key, "")).strip()
    if not raw_path:
        raise ValueError(f"Missing {key} for manifest row {row.get('id', '<unknown>')}")
    candidate = Path(raw_path)
    root_dir = out_dir or OUT_DIR
    if not candidate.is_absolute():
        candidate = root_dir / candidate
    resolved = resolve_for_safety(candidate)
    root = resolve_for_safety(root_dir)
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise ValueError(f"Manifest audio path is outside dataset output: {resolved}") from exc
    if not resolved.is_file():
        raise FileNotFoundError(f"Manifest audio file not found: {resolved}")
    return resolved


def write_jsonl(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


def write_ljspeech(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            text = row["text"].replace("\n", " ").strip()
            f.write(f"{row['id']}|{text}|{text}\n")


def stable_score(row: dict) -> int:
    value = f"{row['source_audio']}::{row['id']}"
    return int(hashlib.sha256(value.encode("utf-8")).hexdigest(), 16)


def split_rows(rows: list[dict]) -> dict[str, list[dict]]:
    by_source: dict[str, list[dict]] = defaultdict(list)
    for row in rows:
        by_source[Path(row["source_audio"]).name].append(row)

    splits = {"train": [], "validation": [], "test": []}
    for _, source_rows in sorted(by_source.items()):
        ordered = sorted(source_rows, key=stable_score)
        n = len(ordered)
        test_n = max(1, round(n * 0.05)) if n >= 20 else 0
        val_n = max(1, round(n * 0.05)) if n >= 20 else 0
        splits["test"].extend(ordered[:test_n])
        splits["validation"].extend(ordered[test_n : test_n + val_n])
        splits["train"].extend(ordered[test_n + val_n :])

    for key in splits:
        splits[key].sort(key=lambda r: r["id"])
    return splits


def duration_stats(rows: list[dict]) -> dict:
    durations = [float(r["duration"]) for r in rows]
    chars = [len(r["text"]) for r in rows]
    cps = [len(r["text"]) / float(r["duration"]) for r in rows]
    if not rows:
        return {}

    def pct(values: list[float], p: float) -> float:
        values = sorted(values)
        idx = min(len(values) - 1, max(0, round((len(values) - 1) * p)))
        return values[idx]

    return {
        "clip_count": len(rows),
        "total_seconds": round(sum(durations), 3),
        "total_minutes": round(sum(durations) / 60, 3),
        "duration_min": round(min(durations), 3),
        "duration_p50": round(pct(durations, 0.50), 3),
        "duration_p95": round(pct(durations, 0.95), 3),
        "duration_max": round(max(durations), 3),
        "text_chars_min": min(chars),
        "text_chars_max": max(chars),
        "char_per_sec_min": round(min(cps), 3),
        "char_per_sec_p50": round(pct(cps, 0.50), 3),
        "char_per_sec_max": round(max(cps), 3),
    }


def write_qc(rows: list[dict], splits: dict[str, list[dict]]) -> dict:
    quality_dir = qc_dir()
    quality_dir.mkdir(parents=True, exist_ok=True)
    summary = {
        "speaker": SPEAKER_ID,
        "trusted_tts": duration_stats(rows),
        "splits": {name: duration_stats(split_rows_) for name, split_rows_ in splits.items()},
        "source_audio_counts": Counter(Path(r["source_audio"]).name for r in rows),
        "source_audio_minutes": {
            source: round(sum(float(r["duration"]) for r in rows if Path(r["source_audio"]).name == source) / 60, 3)
            for source in sorted(set(Path(r["source_audio"]).name for r in rows))
        },
    }
    (quality_dir / "trusted_tts_quality_summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    with (quality_dir / "trusted_tts_clip_stats.csv").open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "id",
                "source_audio",
                "duration",
                "text_chars",
                "char_per_sec",
                "audio_filepath",
                "text",
            ],
        )
        writer.writeheader()
        for row in rows:
            duration = float(row["duration"])
            writer.writerow(
                {
                    "id": row["id"],
                    "source_audio": Path(row["source_audio"]).name,
                    "duration": round(duration, 3),
                    "text_chars": len(row["text"]),
                    "char_per_sec": round(len(row["text"]) / duration, 3),
                    "audio_filepath": row["audio_filepath"],
                    "text": row["text"],
                }
            )
    return summary


def write_dataset_card(summary: dict) -> None:
    trusted = summary["trusted_tts"]
    source_counts = "\n".join(
        f"- {source}: {count} clips, {summary['source_audio_minutes'][source]} minutes"
        for source, count in summary["source_audio_counts"].items()
    )
    split_lines = "\n".join(
        f"- {name}: {stats.get('clip_count', 0)} clips, {stats.get('total_minutes', 0)} minutes"
        for name, stats in summary["splits"].items()
    )
    text = f"""# Halwest Voice Dataset Card

Speaker: `{SPEAKER_ID}`

## Trusted TTS / Voice-Cloning Set

- Clips: {trusted['clip_count']}
- Total duration: {trusted['total_minutes']} minutes
- Sample rate: 24 kHz mono PCM WAV
- Text: Sorani Kurdish in Arabic script from provided transcript files
- Clip duration range: {trusted['duration_min']}s to {trusted['duration_max']}s
- Median clip duration: {trusted['duration_p50']}s

## Splits

{split_lines}

## Source Coverage

{source_counts}

## Known Gaps

- `B7876RX.wav` has no trusted source transcript and is not included in trusted TTS/voice-cloning splits.
- A draft ASR review package exists at `asr_draft/B7876RX/`; it must be corrected by a human before promotion.
- Rows in `review/segment_review.csv` marked `review` should be listened to before final production training.

## Main Files

- `tts_voice_cloning/metadata.csv`: LJSpeech-style trusted metadata.
- `tts_voice_cloning/manifest.jsonl`: trusted JSONL manifest.
- `tts_voice_cloning/splits/`: train, validation, and test manifests.
- `voice_cloning_reference/reference_manifest.csv`: selected reference clips.
- `quality_control/trusted_tts_quality_summary.json`: numeric QC summary.
- `quality_control/trusted_tts_audio_qc.csv`: per-clip audio integrity checks.
- `review_pages/index.html`: browser review pages with audio players and transcript text.
"""
    (OUT_DIR / "DATASET_CARD.md").write_text(text, encoding="utf-8")


def update_readme() -> None:
    readme = OUT_DIR / "README.md"
    if not readme.exists():
        return
    text = readme.read_text(encoding="utf-8")
    marker = "## Training Splits And QC"
    addition = """## Training Splits And QC

- `tts_voice_cloning/splits/`: deterministic trusted train/validation/test splits.
- `quality_control/`: duration, character-rate, source-coverage, and split summaries.
- `DATASET_CARD.md`: concise training handoff notes.
- `review_pages/index.html`: browser pages for trusted transcript review, flagged segment review, and B7876 correction.
"""
    if marker not in text:
        readme.write_text(text.rstrip() + "\n\n" + addition, encoding="utf-8")


def main() -> None:
    manifest = tts_dir() / "manifest.jsonl"
    if not manifest.exists():
        raise FileNotFoundError(manifest)
    rows = read_jsonl(manifest)
    for row in rows:
        checked_manifest_audio_path(row)

    splits = split_rows(rows)
    for name, split in splits.items():
        write_jsonl(splits_dir() / f"{name}.jsonl", split)
        write_ljspeech(splits_dir() / f"metadata_{name}.csv", split)

    summary = write_qc(rows, splits)
    write_dataset_card(summary)
    update_readme()
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
