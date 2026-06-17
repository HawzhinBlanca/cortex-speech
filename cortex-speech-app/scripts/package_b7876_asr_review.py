import csv
import subprocess
from pathlib import Path

from build_halwest_dataset import OUT_DIR
from promote_b7876_corrections import valid_row_id


ASR_DIR = OUT_DIR / "asr_draft" / "B7876RX"
SEGMENTS_CSV = ASR_DIR / "B7876RX_draft_segments.csv"
REVIEW_WAVS = ASR_DIR / "review_wavs"
CORRECTION_TEMPLATE = ASR_DIR / "B7876RX_correction_template.csv"
_DEFAULT_ASR_DIR = ASR_DIR
_DEFAULT_SEGMENTS_CSV = SEGMENTS_CSV
_DEFAULT_REVIEW_WAVS = REVIEW_WAVS
_DEFAULT_CORRECTION_TEMPLATE = CORRECTION_TEMPLATE


def asr_dir() -> Path:
    if ASR_DIR != _DEFAULT_ASR_DIR:
        return ASR_DIR
    return OUT_DIR / "asr_draft" / "B7876RX"


def segments_csv() -> Path:
    default = asr_dir() / "B7876RX_draft_segments.csv"
    return SEGMENTS_CSV if SEGMENTS_CSV != _DEFAULT_SEGMENTS_CSV else default


def review_wavs() -> Path:
    default = asr_dir() / "review_wavs"
    return REVIEW_WAVS if REVIEW_WAVS != _DEFAULT_REVIEW_WAVS else default


def correction_template() -> Path:
    default = asr_dir() / "B7876RX_correction_template.csv"
    return CORRECTION_TEMPLATE if CORRECTION_TEMPLATE != _DEFAULT_CORRECTION_TEMPLATE else default


def run(cmd: list[str]) -> None:
    subprocess.run(cmd, check=True, text=True, capture_output=True)


def main() -> None:
    segment_path = segments_csv()
    if not segment_path.exists():
        raise FileNotFoundError(segment_path)

    rows: list[dict] = []
    with segment_path.open("r", encoding="utf-8-sig", newline="") as f:
        reader = csv.DictReader(f)
        rows = list(reader)

    wav_dir = review_wavs()
    wav_dir.mkdir(parents=True, exist_ok=True)
    packaged: list[dict] = []

    for row in rows:
        row_id = row["id"].strip()
        if not valid_row_id(row_id):
            raise ValueError(f"Unsafe B7876 segment id: {row['id']}")
        clip = wav_dir / f"{row_id}.wav"
        if not clip.exists():
            run(
                [
                    "ffmpeg",
                    "-hide_banner",
                    "-y",
                    "-ss",
                    row["start_sec"],
                    "-to",
                    row["end_sec"],
                    "-i",
                    row["source_audio"],
                    "-ac",
                    "1",
                    "-ar",
                    "24000",
                    "-sample_fmt",
                    "s16",
                    "-af",
                    "highpass=f=60,lowpass=f=11500,loudnorm=I=-20:TP=-2:LRA=11",
                    str(clip),
                ]
            )
        packaged.append(
            {
                "id": row["id"],
                "review_audio_file": str(clip),
                "start_sec": row["start_sec"],
                "end_sec": row["end_sec"],
                "duration_sec": row["duration_sec"],
                "draft_text": row["text"],
                "corrected_text": "",
                "review_status": "needs_human_correction",
                "notes": "",
            }
        )

    template = correction_template()
    with template.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "id",
                "review_audio_file",
                "start_sec",
                "end_sec",
                "duration_sec",
                "draft_text",
                "corrected_text",
                "review_status",
                "notes",
            ],
        )
        writer.writeheader()
        writer.writerows(packaged)

    readme = asr_dir() / "README.md"
    existing = readme.read_text(encoding="utf-8") if readme.exists() else ""
    addition = (
        "\nReview packaging:\n"
        f"- `review_wavs/`: {len(packaged)} normalized per-chunk WAV files for manual checking.\n"
        "- `B7876RX_correction_template.csv`: fill `corrected_text` and set `review_status=approved` when each row is verified.\n"
    )
    if "B7876RX_correction_template.csv" not in existing:
        readme.write_text(existing.rstrip() + "\n" + addition, encoding="utf-8")

    print(f"packaged_rows={len(packaged)}")
    print(str(template))


if __name__ == "__main__":
    main()
