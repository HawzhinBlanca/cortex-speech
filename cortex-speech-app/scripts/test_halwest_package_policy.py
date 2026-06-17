import csv
import tempfile
from pathlib import Path

import package_b7876_asr_review as package


def write_segments_csv(path: Path, row_id: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=["id", "source_audio", "start_sec", "end_sec", "duration_sec", "text"],
        )
        writer.writeheader()
        writer.writerow(
            {
                "id": row_id,
                "source_audio": str(path.parent / "source.wav"),
                "start_sec": "0",
                "end_sec": "1",
                "duration_sec": "1",
                "text": "draft",
            }
        )


def test_package_refuses_unsafe_segment_ids() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        asr_dir = root / "out" / "asr_draft" / "B7876RX"
        segments_csv = asr_dir / "B7876RX_draft_segments.csv"
        write_segments_csv(segments_csv, "../escape")

        original_asr_dir = package.ASR_DIR
        original_segments_csv = package.SEGMENTS_CSV
        original_review_wavs = package.REVIEW_WAVS
        original_template = package.CORRECTION_TEMPLATE
        try:
            package.ASR_DIR = asr_dir
            package.SEGMENTS_CSV = segments_csv
            package.REVIEW_WAVS = asr_dir / "review_wavs"
            package.CORRECTION_TEMPLATE = asr_dir / "B7876RX_correction_template.csv"
            try:
                package.main()
            except ValueError as exc:
                assert "Unsafe B7876 segment id" in str(exc)
            else:
                raise AssertionError("package_b7876_asr_review accepted an unsafe segment id")
            assert not (asr_dir / "escape.wav").exists()
            assert not (root / "out" / "asr_draft" / "escape.wav").exists()
        finally:
            package.ASR_DIR = original_asr_dir
            package.SEGMENTS_CSV = original_segments_csv
            package.REVIEW_WAVS = original_review_wavs
            package.CORRECTION_TEMPLATE = original_template


def test_package_paths_follow_current_out_dir_without_derived_overrides() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        asr_dir = out_dir / "asr_draft" / "B7876RX"
        segments_csv = asr_dir / "B7876RX_draft_segments.csv"
        write_segments_csv(segments_csv, "../escape")

        original_out = package.OUT_DIR
        try:
            package.OUT_DIR = out_dir
            assert package.asr_dir() == asr_dir
            assert package.segments_csv() == segments_csv
            assert package.review_wavs() == asr_dir / "review_wavs"
            assert package.correction_template() == asr_dir / "B7876RX_correction_template.csv"
        finally:
            package.OUT_DIR = original_out


def main() -> None:
    test_package_refuses_unsafe_segment_ids()
    test_package_paths_follow_current_out_dir_without_derived_overrides()
    print("halwest package policy regression passed")


if __name__ == "__main__":
    main()
