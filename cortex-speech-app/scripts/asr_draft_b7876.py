import csv
import json
import re
import subprocess
import wave
from pathlib import Path

import numpy as np

from build_halwest_dataset import (
    MAX_SEG_DUR,
    MIN_SEG_DUR,
    SOURCE_DIR,
    OUT_DIR,
    build_speech_intervals,
    detect_silences,
    ffprobe_duration,
)


SOURCE_AUDIO = SOURCE_DIR / "B7876RX.wav"
ASR_DIR = OUT_DIR / "asr_draft" / "B7876RX"
ASR_INPUT = ASR_DIR / "B7876RX_16k_asr_input.wav"
MODEL_NAME = "openai/whisper-small"
LANGUAGE = "arabic"
_DEFAULT_SOURCE_AUDIO = SOURCE_AUDIO
_DEFAULT_ASR_DIR = ASR_DIR
_DEFAULT_ASR_INPUT = ASR_INPUT


def source_audio() -> Path:
    if SOURCE_AUDIO != _DEFAULT_SOURCE_AUDIO:
        return SOURCE_AUDIO
    return SOURCE_DIR / "B7876RX.wav"


def asr_dir() -> Path:
    if ASR_DIR != _DEFAULT_ASR_DIR:
        return ASR_DIR
    return OUT_DIR / "asr_draft" / "B7876RX"


def asr_input() -> Path:
    if ASR_INPUT != _DEFAULT_ASR_INPUT:
        return ASR_INPUT
    return asr_dir() / "B7876RX_16k_asr_input.wav"


def load_asr_pipeline():
    try:
        from transformers import pipeline
    except ImportError as exc:
        raise RuntimeError("Missing dependency: install transformers before running B7876 ASR draft generation.") from exc
    return pipeline("automatic-speech-recognition", model=MODEL_NAME, device=-1)


def run(cmd: list[str]) -> None:
    subprocess.run(cmd, check=True, text=True, capture_output=True)


def convert_asr_input() -> None:
    source = source_audio()
    target = asr_input()
    target.parent.mkdir(parents=True, exist_ok=True)
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
            "16000",
            "-sample_fmt",
            "s16",
            "-af",
            "highpass=f=60,lowpass=f=7800,loudnorm=I=-20:TP=-2:LRA=11",
            str(target),
        ]
    )


def load_audio(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as wav:
        sample_rate = wav.getframerate()
        audio = np.frombuffer(wav.readframes(wav.getnframes()), dtype=np.int16)
    return audio.astype(np.float32) / 32768.0, sample_rate


def build_asr_chunks(duration: float) -> list[tuple[float, float]]:
    speech = build_speech_intervals(duration, detect_silences(source_audio()))
    chunks: list[tuple[float, float]] = []
    start = None
    end = None
    target = 18.0
    max_dur = 24.0

    for seg_start, seg_end in speech:
        if start is None:
            start, end = seg_start, seg_end
            continue
        assert end is not None
        proposed = seg_end - start
        current = end - start
        if proposed <= max_dur and current < target:
            end = seg_end
        else:
            if end - start >= MIN_SEG_DUR:
                chunks.append((start, end))
            start, end = seg_start, seg_end

    if start is not None and end is not None and end - start >= MIN_SEG_DUR:
        chunks.append((start, end))

    return [(max(0.0, s), min(duration, e)) for s, e in chunks if e - s <= max(MAX_SEG_DUR * 2.5, 24.0)]


def clean_asr_text(text: str) -> str:
    text = re.sub(r"\s+", " ", text).strip()
    words = text.split()
    cleaned: list[str] = []
    repeat_count = 0
    last = None
    for word in words:
        if word == last:
            repeat_count += 1
            if repeat_count >= 3:
                continue
        else:
            repeat_count = 0
        cleaned.append(word)
        last = word
    return " ".join(cleaned)


def main() -> None:
    source = source_audio()
    if not source.exists():
        raise FileNotFoundError(source)

    convert_asr_input()
    duration = ffprobe_duration(source)
    chunks = build_asr_chunks(duration)
    audio, sample_rate = load_audio(asr_input())

    asr = load_asr_pipeline()
    rows: list[dict] = []
    texts: list[str] = []

    for idx, (start, end) in enumerate(chunks, start=1):
        start_i = int(start * sample_rate)
        end_i = int(end * sample_rate)
        sample = audio[start_i:end_i]
        result = asr(
            {"array": sample, "sampling_rate": sample_rate},
            generate_kwargs={
                "task": "transcribe",
                "language": LANGUAGE,
                "no_repeat_ngram_size": 6,
            },
        )
        raw_text = result.get("text", "")
        text = clean_asr_text(raw_text)
        row = {
            "id": f"B7876RX_asr_{idx:04d}",
            "source_audio": str(source),
            "start_sec": round(start, 3),
            "end_sec": round(end, 3),
            "duration_sec": round(end - start, 3),
            "asr_model": MODEL_NAME,
            "forced_language": LANGUAGE,
            "status": "draft_needs_human_correction",
            "text": text,
            "raw_text": raw_text,
        }
        rows.append(row)
        if text:
            texts.append(text)
        print(f"{idx}/{len(chunks)} {start:.2f}-{end:.2f} {text[:80]}", flush=True)

    output_dir = asr_dir()
    csv_path = output_dir / "B7876RX_draft_segments.csv"
    with csv_path.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "id",
                "source_audio",
                "start_sec",
                "end_sec",
                "duration_sec",
                "asr_model",
                "forced_language",
                "status",
                "text",
                "raw_text",
            ],
        )
        writer.writeheader()
        writer.writerows(rows)

    jsonl_path = output_dir / "B7876RX_draft_manifest.jsonl"
    with jsonl_path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    txt_path = output_dir / "B7876RX_draft_transcript.txt"
    txt_path.write_text("\n".join(texts) + "\n", encoding="utf-8")

    readme = output_dir / "README.md"
    readme.write_text(
        "\n".join(
            [
                "# B7876RX ASR Draft",
                "",
                "This folder contains a draft ASR transcript for `B7876RX.wav`.",
                "",
                "Do not use this text as final TTS or voice-cloning labels without human correction.",
                "The local model was forced to Arabic because Whisper does not expose Kurdish as a supported language in this runtime.",
                "The output is useful for locating spoken content and speeding up manual transcription, but it is not a perfect Sorani transcript.",
                "",
                "Files:",
                "- `B7876RX_draft_segments.csv`: timestamped draft text, one row per ASR chunk.",
                "- `B7876RX_draft_manifest.jsonl`: same data for scripts.",
                "- `B7876RX_draft_transcript.txt`: concatenated draft transcript.",
                "- `B7876RX_16k_asr_input.wav`: normalized 16 kHz ASR input copy.",
                "",
                f"Chunks: {len(rows)}",
                f"Total source duration seconds: {duration:.3f}",
                f"Model: {MODEL_NAME}",
                f"Forced language: {LANGUAGE}",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    print(json.dumps({"chunks": len(rows), "out_dir": str(output_dir)}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
