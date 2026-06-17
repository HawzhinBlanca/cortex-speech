import argparse
import csv
import json
import re
import shutil
from pathlib import Path

from build_halwest_dataset import OUT_DIR, SPEAKER_ID, resolve_for_safety
from finalize_halwest_dataset import main as finalize_dataset


ASR_DIR = OUT_DIR / "asr_draft" / "B7876RX"
CORRECTION_TEMPLATE = ASR_DIR / "B7876RX_correction_template.csv"
TRUSTED_DIR = OUT_DIR / "tts_voice_cloning"
PROMOTION_REPORT = ASR_DIR / "B7876RX_promotion_report.json"
SAFE_ROW_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]*$")
_DEFAULT_ASR_DIR = ASR_DIR
_DEFAULT_CORRECTION_TEMPLATE = CORRECTION_TEMPLATE
_DEFAULT_TRUSTED_DIR = TRUSTED_DIR
_DEFAULT_PROMOTION_REPORT = PROMOTION_REPORT


def asr_dir() -> Path:
    if ASR_DIR != _DEFAULT_ASR_DIR:
        return ASR_DIR
    return OUT_DIR / "asr_draft" / "B7876RX"


def correction_template() -> Path:
    default = asr_dir() / "B7876RX_correction_template.csv"
    return CORRECTION_TEMPLATE if CORRECTION_TEMPLATE != _DEFAULT_CORRECTION_TEMPLATE else default


def trusted_dir() -> Path:
    if TRUSTED_DIR != _DEFAULT_TRUSTED_DIR:
        return TRUSTED_DIR
    return OUT_DIR / "tts_voice_cloning"


def promotion_report() -> Path:
    default = asr_dir() / "B7876RX_promotion_report.json"
    return PROMOTION_REPORT if PROMOTION_REPORT != _DEFAULT_PROMOTION_REPORT else default


def valid_row_id(row_id: str) -> bool:
    return bool(SAFE_ROW_ID.fullmatch(row_id))


def read_rows() -> list[dict]:
    template = correction_template()
    if not template.exists():
        raise FileNotFoundError(template)
    with template.open("r", encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def trusted_review_audio_path(row: dict) -> Path | None:
    raw_path = row.get("review_audio_file", "").strip()
    if not raw_path:
        return None
    path = resolve_for_safety(Path(raw_path))
    review_root = resolve_for_safety(asr_dir())
    try:
        path.relative_to(review_root)
    except ValueError:
        return None
    if path.suffix.lower() != ".wav" or not path.is_file():
        return None
    return path


def approved_rows(rows: list[dict]) -> list[dict]:
    approved: list[dict] = []
    for row in rows:
        audio_path = trusted_review_audio_path(row)
        if (
            row.get("review_status", "").strip().lower() == "approved"
            and row.get("corrected_text", "").strip()
            and audio_path
        ):
            copied = dict(row)
            copied["review_audio_file"] = str(audio_path)
            approved.append(copied)
    return approved


def write_ljspeech_metadata(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8", newline="\n") as f:
        for row in rows:
            text = (row.get("corrected_text") or row["text"]).replace("\n", " ").strip()
            f.write(f"{row['id']}|{text}|{text}\n")


def load_manifest(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def write_manifest(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


def promote(rows: list[dict]) -> dict:
    trusted = trusted_dir()
    wav_dir = trusted / "wavs"
    manifest_path = trusted / "manifest.jsonl"
    metadata_path = trusted / "metadata.csv"
    existing = load_manifest(manifest_path)
    existing_ids = {row["id"] for row in existing}
    seen_ids = set(existing_ids)

    promoted_manifest_rows: list[dict] = []
    skipped_existing = 0
    skipped_duplicate = 0
    skipped_invalid_audio = 0
    for row in rows:
        row_id = row.get("id", "").strip()
        if not row_id or not valid_row_id(row_id):
            skipped_duplicate += 1
            continue
        if row_id in existing_ids:
            skipped_existing += 1
            continue
        if row_id in seen_ids:
            skipped_duplicate += 1
            continue
        src = trusted_review_audio_path(row)
        if not src:
            skipped_invalid_audio += 1
            continue
        wav_dir.mkdir(parents=True, exist_ok=True)
        dest = wav_dir / f"{row_id}.wav"
        shutil.copy2(src, dest)
        seen_ids.add(row_id)
        promoted_manifest_rows.append(
            {
                "id": row_id,
                "speaker": SPEAKER_ID,
                "audio_filepath": str(dest),
                "source_audio": str(OUT_DIR / "source_assets" / "B7876RX.wav"),
                "offset": float(row["start_sec"]),
                "duration": float(row["duration_sec"]),
                "text": row["corrected_text"].strip(),
                "review_source": str(correction_template()),
            }
        )

    if promoted_manifest_rows:
        write_manifest(manifest_path, existing + promoted_manifest_rows)
        write_ljspeech_metadata(metadata_path, promoted_manifest_rows)
        finalize_dataset()

    report = {
        "approved_rows_seen": len(rows),
        "new_rows_promoted": len(promoted_manifest_rows),
        "skipped_existing_ids": skipped_existing,
        "skipped_duplicate_ids": skipped_duplicate,
        "skipped_invalid_audio": skipped_invalid_audio,
        "trusted_manifest": str(manifest_path),
        "trusted_metadata": str(metadata_path),
    }
    promotion_report().write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--apply", action="store_true", help="Promote approved corrected rows into the trusted TTS set.")
    args = parser.parse_args()

    rows = read_rows()
    approved = approved_rows(rows)
    report = {
        "correction_rows": len(rows),
        "approved_ready_rows": len(approved),
        "not_ready_rows": len(rows) - len(approved),
        "apply": args.apply,
    }
    if args.apply:
        report.update(promote(approved))
    else:
        promotion_report().write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
