import csv
import json
import math
import wave
from collections import Counter
from pathlib import Path

import numpy as np

from build_halwest_dataset import OUT_DIR
from finalize_halwest_dataset import checked_manifest_audio_path


def qc_dir() -> Path:
    return OUT_DIR / "quality_control"


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def wav_stats(path: Path) -> dict:
    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        sample_rate = wav.getframerate()
        sample_width = wav.getsampwidth()
        frames = wav.getnframes()
        raw = wav.readframes(frames)
    if sample_width != 2:
        raise ValueError(f"Expected 16-bit PCM WAV: {path}")
    data = np.frombuffer(raw, dtype=np.int16).astype(np.float64)
    if channels > 1:
        data = data.reshape(-1, channels).mean(axis=1)
    if data.size == 0:
        peak = rms = 0.0
        clipped = 0
    else:
        abs_data = np.abs(data)
        peak = float(abs_data.max()) / 32768.0
        rms = math.sqrt(float(np.mean(data * data))) / 32768.0
        clipped = int(np.sum(abs_data >= 32760))
    duration = frames / sample_rate if sample_rate else 0.0
    peak_dbfs = 20 * math.log10(peak) if peak > 0 else -120.0
    rms_dbfs = 20 * math.log10(rms) if rms > 0 else -120.0
    return {
        "sample_rate": sample_rate,
        "channels": channels,
        "sample_width": sample_width,
        "frames": frames,
        "actual_duration_sec": round(duration, 3),
        "peak_dbfs": round(peak_dbfs, 3),
        "rms_dbfs": round(rms_dbfs, 3),
        "clipped_samples": clipped,
    }


def issues_for(row: dict, stats: dict) -> list[str]:
    issues: list[str] = []
    expected = float(row["duration"])
    actual = float(stats["actual_duration_sec"])
    if abs(expected - actual) > 0.08:
        issues.append("duration_mismatch")
    if stats["sample_rate"] != 24000:
        issues.append("sample_rate_not_24k")
    if stats["channels"] != 1:
        issues.append("not_mono")
    if stats["clipped_samples"] > 0:
        issues.append("possible_clipping")
    if stats["peak_dbfs"] > -0.5:
        issues.append("peak_too_hot")
    if stats["peak_dbfs"] < -18:
        issues.append("peak_too_low")
    if stats["rms_dbfs"] < -35:
        issues.append("rms_too_low")
    if stats["rms_dbfs"] > -10:
        issues.append("rms_too_hot")
    return issues


def qc_manifest_row(row: dict) -> dict:
    try:
        path = checked_manifest_audio_path(row, out_dir=OUT_DIR)
        stats = wav_stats(path)
        issues = issues_for(row, stats)
    except FileNotFoundError:
        stats = {}
        issues = ["missing_audio"]
    except ValueError as exc:
        stats = {}
        issues = [str(exc)]
    return {
        "id": row["id"],
        "audio_filepath": row["audio_filepath"],
        "expected_duration_sec": row["duration"],
        **stats,
        "issues": ";".join(issues),
    }


def main() -> None:
    manifest = OUT_DIR / "tts_voice_cloning" / "manifest.jsonl"
    rows = read_jsonl(manifest)
    quality_dir = qc_dir()
    quality_dir.mkdir(parents=True, exist_ok=True)
    qc_rows: list[dict] = []
    issue_counter: Counter[str] = Counter()

    for row in rows:
        qc_row = qc_manifest_row(row)
        if qc_row["issues"]:
            issue_counter.update(qc_row["issues"].split(";"))
        qc_rows.append(qc_row)

    csv_path = quality_dir / "trusted_tts_audio_qc.csv"
    fieldnames = [
        "id",
        "audio_filepath",
        "expected_duration_sec",
        "sample_rate",
        "channels",
        "sample_width",
        "frames",
        "actual_duration_sec",
        "peak_dbfs",
        "rms_dbfs",
        "clipped_samples",
        "issues",
    ]
    with csv_path.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(qc_rows)

    summary = {
        "checked_clips": len(qc_rows),
        "clips_with_issues": sum(1 for row in qc_rows if row["issues"]),
        "issue_counts": dict(issue_counter),
        "audio_qc_csv": str(csv_path),
    }
    (quality_dir / "trusted_tts_audio_qc_summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
