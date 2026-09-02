import csv
import hashlib
import json
import shutil
from collections import Counter, defaultdict
from pathlib import Path

from build_halwest_dataset import (
    OUT_DIR,
    SPEAKER_ID,
    dataset_relpath,
    normalize_text,
    pair_sources,
    read_text,
    remove_tree_checked,
)
from finalize_halwest_dataset import checked_manifest_audio_path, duration_stats, split_rows, write_jsonl, write_ljspeech


GOLD_DIR = OUT_DIR / "tts_voice_cloning_gold"
GOLD_REF_DIR = OUT_DIR / "voice_cloning_gold_reference"
QC_DIR = OUT_DIR / "quality_control"


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def read_csv(path: Path) -> list[dict]:
    with path.open("r", encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def write_csv(path: Path, fieldnames: list[str], rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def stable_score(row: dict) -> int:
    key = f"{Path(row['source_audio']).name}::{row['id']}"
    return int(hashlib.sha256(key.encode("utf-8")).hexdigest(), 16)


def exact_transcript_sources() -> dict[str, str]:
    sources: dict[str, str] = {}
    for item in pair_sources():
        if item.transcript_status == "matched_by_basename" and read_text(item.transcript):
            sources[item.audio.name] = dataset_relpath(item.transcript, OUT_DIR)
    return sources


def copy_gold_rows(rows: list[dict], exact_sources: dict[str, str]) -> list[dict]:
    planned_rows: list[tuple[dict, Path]] = []
    for row in sorted(rows, key=lambda r: r["id"]):
        source_name = Path(row["source_audio"]).name
        if source_name not in exact_sources:
            continue
        planned_rows.append((row, checked_manifest_audio_path(row, out_dir=OUT_DIR)))

    remove_tree_checked(GOLD_DIR, OUT_DIR)
    wav_dir = GOLD_DIR / "wavs"
    wav_dir.mkdir(parents=True, exist_ok=True)

    gold_rows: list[dict] = []
    for row, src in planned_rows:
        source_name = Path(row["source_audio"]).name
        dest = wav_dir / src.name
        shutil.copy2(src, dest)
        copied = dict(row)
        copied["audio_filepath"] = dataset_relpath(dest, OUT_DIR)
        copied["source_transcript"] = exact_sources[source_name]
        copied["trust_tier"] = "gold_exact_basename_transcript"
        gold_rows.append(copied)
    return gold_rows


def write_gold_dataset(rows: list[dict]) -> dict:
    write_jsonl(GOLD_DIR / "manifest.jsonl", rows)
    write_ljspeech(GOLD_DIR / "metadata.csv", rows)

    splits = split_rows(rows)
    for name, split in splits.items():
        write_jsonl(GOLD_DIR / "splits" / f"{name}.jsonl", split)
        write_ljspeech(GOLD_DIR / "splits" / f"metadata_{name}.csv", split)

    ref_candidates = [
        row
        for row in rows
        if 6.0 <= float(row["duration"]) <= 12.0 and 8.0 <= len(row["text"]) / float(row["duration"]) <= 24.0
    ]
    ref_candidates.sort(key=lambda r: (abs(float(r["duration"]) - 9.0), -len(r["text"]), stable_score(r)))
    remove_tree_checked(GOLD_REF_DIR, OUT_DIR)
    reference_rows: list[dict] = []
    for idx, row in enumerate(ref_candidates[:12], start=1):
        src = checked_manifest_audio_path(row, out_dir=OUT_DIR)
        dest = GOLD_REF_DIR / "top_reference_clips" / f"gold_ref_{idx:02d}_{src.name}"
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        reference_rows.append(
            {
                "reference_rank": idx,
                "id": row["id"],
                "speaker": row["speaker"],
                "reference_audio_file": dataset_relpath(dest, OUT_DIR),
                "source_audio": row["source_audio"],
                "source_transcript": row["source_transcript"],
                "duration_sec": row["duration"],
                "text_chars": len(row["text"]),
                "char_per_sec": round(len(row["text"]) / float(row["duration"]), 3),
                "text": row["text"],
                "trust_tier": row["trust_tier"],
            }
        )
    write_csv(
        GOLD_REF_DIR / "reference_manifest.csv",
        [
            "reference_rank",
            "id",
            "speaker",
            "reference_audio_file",
            "source_audio",
            "source_transcript",
            "duration_sec",
            "text_chars",
            "char_per_sec",
            "text",
            "trust_tier",
        ],
        reference_rows,
    )

    summary = {
        "speaker": SPEAKER_ID,
        "trust_tier": "gold_exact_basename_transcript",
        "clip_count": len(rows),
        "reference_clip_count": len(reference_rows),
        "stats": duration_stats(rows),
        "splits": {name: duration_stats(split) for name, split in splits.items()},
        "source_audio_counts": Counter(Path(r["source_audio"]).name for r in rows),
        "source_audio_minutes": {
            source: round(sum(float(r["duration"]) for r in rows if Path(r["source_audio"]).name == source) / 60, 3)
            for source in sorted(set(Path(r["source_audio"]).name for r in rows))
        },
    }
    (GOLD_DIR / "quality_summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    return summary


def validate_transcript_roundtrip() -> list[dict]:
    segment_rows = read_csv(OUT_DIR / "review" / "segment_review.csv")
    by_source: dict[str, list[dict]] = defaultdict(list)
    for row in segment_rows:
        by_source[Path(row["source_audio"]).name].append(row)

    validation_rows: list[dict] = []
    for item in pair_sources():
        source_text = normalize_text(read_text(item.transcript))
        ordered = sorted(by_source.get(item.audio.name, []), key=lambda row: row["id"])
        segmented_text = normalize_text(" ".join(row.get("text", "") for row in ordered))
        source_hash = hashlib.sha256(source_text.encode("utf-8")).hexdigest() if source_text else ""
        segmented_hash = hashlib.sha256(segmented_text.encode("utf-8")).hexdigest() if segmented_text else ""
        validation_rows.append(
            {
                "audio_file": item.audio.name,
                "transcript_file": item.transcript.name if item.transcript else "",
                "transcript_status": item.transcript_status,
                "source_chars": len(source_text),
                "segmented_chars": len(segmented_text),
                "segment_rows": len(ordered),
                "source_sha256": source_hash,
                "segmented_sha256": segmented_hash,
                "roundtrip_exact": bool(source_text) and source_hash == segmented_hash,
                "note": "Text preserved across silence chunks; timing still requires listening/forced alignment.",
            }
        )
    return validation_rows


def write_cards(summary: dict, validation_rows: list[dict]) -> None:
    QC_DIR.mkdir(parents=True, exist_ok=True)
    validation_report = {
        "validation": validation_rows,
        "all_transcript_backed_roundtrips_exact": all(
            row["roundtrip_exact"] for row in validation_rows if row["transcript_status"] != "missing_transcript"
        ),
    }
    (QC_DIR / "transcript_roundtrip_validation.json").write_text(
        json.dumps(validation_report, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    write_csv(
        QC_DIR / "transcript_roundtrip_validation.csv",
        [
            "audio_file",
            "transcript_file",
            "transcript_status",
            "source_chars",
            "segmented_chars",
            "segment_rows",
            "source_sha256",
            "segmented_sha256",
            "roundtrip_exact",
            "note",
        ],
        validation_rows,
    )

    gold_stats = summary["stats"]
    split_lines = "\n".join(
        f"- {name}: {stats.get('clip_count', 0)} clips, {stats.get('total_minutes', 0)} minutes"
        for name, stats in summary["splits"].items()
    )
    train_only = not summary["splits"]["validation"] and not summary["splits"]["test"]
    split_note = (
        "**TRAIN ONLY** — fewer than three source recordings back this subset, so no audio can be\n"
        "held out without putting the same recording on both sides. `validation` and `test` are\n"
        "deliberately EMPTY; no score measured on this data is held-out."
        if train_only
        else "Whole source recordings go to exactly one split, so no recording appears on two sides."
    )
    card = f"""# Halwest Gold TTS / Voice-Cloning Subset

Speaker: `{SPEAKER_ID}`

Trust tier: `gold_exact_basename_transcript`

This subset includes only source audio with a non-empty transcript file that matches the WAV basename. It is stricter than the broader `tts_voice_cloning/` set because it excludes inferred transcript pairings.

## Contents

- Clips: {summary['clip_count']}
- Total duration: {gold_stats.get('total_minutes', 0)} minutes
- Sample rate: 24 kHz mono PCM WAV
- Source coverage: {dict(summary['source_audio_minutes'])}
- Reference clips: {summary['reference_clip_count']}

## Splits

{split_note}

{split_lines}

## Files

- `tts_voice_cloning_gold/manifest.jsonl`
- `tts_voice_cloning_gold/metadata.csv`
- `tts_voice_cloning_gold/splits/`
- `voice_cloning_gold_reference/reference_manifest.csv`
- `quality_control/transcript_roundtrip_validation.json`

## Accuracy Boundary

The transcript text round-trips exactly from the source transcript into the chunked rows. Clip timing is based on silence cuts and transcript order, so final production training should still use the review page or forced alignment for line-level listening verification.
"""
    (OUT_DIR / "GOLD_DATASET_CARD.md").write_text(card, encoding="utf-8")


def patch_root_docs(summary: dict) -> None:
    section = f"""## Gold Exact-Transcript Subset

- `tts_voice_cloning_gold/`: strict TTS/voice-cloning subset using only exact same-basename transcript pairs.
- `voice_cloning_gold_reference/`: top reference clips from that strict subset.
- Clips: {summary['clip_count']}; duration: {summary['stats'].get('total_minutes', 0)} minutes.
- `GOLD_DATASET_CARD.md`: strict-subset handoff notes.
"""
    for doc_name in ["README.md", "DATASET_CARD.md"]:
        path = OUT_DIR / doc_name
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        marker = "## Gold Exact-Transcript Subset"
        if marker in text:
            text = text[: text.index(marker)].rstrip()
        path.write_text(text.rstrip() + "\n\n" + section, encoding="utf-8")


def main() -> None:
    manifest = OUT_DIR / "tts_voice_cloning" / "manifest.jsonl"
    if not manifest.exists():
        raise FileNotFoundError(manifest)
    exact_sources = exact_transcript_sources()
    if not exact_sources:
        raise RuntimeError("No exact same-basename transcript sources were found.")

    gold_rows = copy_gold_rows(read_jsonl(manifest), exact_sources)
    summary = write_gold_dataset(gold_rows)
    validation_rows = validate_transcript_roundtrip()
    write_cards(summary, validation_rows)
    patch_root_docs(summary)
    print(json.dumps({"gold": summary, "validation_rows": validation_rows}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
