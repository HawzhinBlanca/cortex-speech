import tempfile
from pathlib import Path

import create_halwest_gold_subset as gold


def write_file(path: Path, content: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_gold_subset_removes_only_under_output_dir() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        gold_dir = out_dir / "tts_voice_cloning_gold"
        ref_dir = out_dir / "voice_cloning_gold_reference"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "source.wav"
        outside_audio = root / "outside.wav"
        write_file(audio, "wav")
        write_file(outside_audio, "wav")
        write_file(gold_dir / "stale.txt", "delete")
        write_file(ref_dir / "stale.txt", "delete")

        original_out = gold.OUT_DIR
        original_gold_dir = gold.GOLD_DIR
        original_ref_dir = gold.GOLD_REF_DIR
        try:
            gold.OUT_DIR = out_dir
            gold.GOLD_DIR = gold_dir
            gold.GOLD_REF_DIR = ref_dir
            rows = [
                {
                    "id": "gold_0001",
                    "speaker": "halwest",
                    "audio_filepath": str(audio),
                    "source_audio": str(root / "Halwest1.wav"),
                    "duration": "7.0",
                    "text": "trusted transcript text",
                }
            ]
            copied = gold.copy_gold_rows(rows, {"Halwest1.wav": str(root / "Halwest1.txt.txt")})
            assert len(copied) == 1
            assert not (gold_dir / "stale.txt").exists()
            assert (gold_dir / "wavs" / "source.wav").exists()

            summary = gold.write_gold_dataset(copied)
            assert summary["clip_count"] == 1
            assert not (ref_dir / "stale.txt").exists()

            bad_rows = [
                {
                    "id": "bad",
                    "speaker": "halwest",
                    "audio_filepath": str(outside_audio),
                    "source_audio": str(root / "Halwest1.wav"),
                    "duration": "7.0",
                    "text": "outside transcript text",
                }
            ]
            try:
                gold.copy_gold_rows(bad_rows, {"Halwest1.wav": str(root / "Halwest1.txt.txt")})
            except ValueError as exc:
                assert "outside dataset output" in str(exc)
            else:
                raise AssertionError("copy_gold_rows accepted manifest audio outside OUT_DIR")
            assert (gold_dir / "wavs" / "source.wav").exists()
        finally:
            gold.OUT_DIR = original_out
            gold.GOLD_DIR = original_gold_dir
            gold.GOLD_REF_DIR = original_ref_dir


def test_gold_subset_refuses_deletes_outside_output_dir() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        outside = root / "outside_gold"
        write_file(outside / "do_not_delete.txt", "keep")

        original_out = gold.OUT_DIR
        original_gold_dir = gold.GOLD_DIR
        try:
            gold.OUT_DIR = out_dir
            gold.GOLD_DIR = outside
            try:
                gold.copy_gold_rows([], {})
            except ValueError as exc:
                assert "outside expected parent" in str(exc)
            else:
                raise AssertionError("copy_gold_rows accepted GOLD_DIR outside OUT_DIR")
            assert (outside / "do_not_delete.txt").exists()
        finally:
            gold.OUT_DIR = original_out
            gold.GOLD_DIR = original_gold_dir


def main() -> None:
    test_gold_subset_removes_only_under_output_dir()
    test_gold_subset_refuses_deletes_outside_output_dir()
    print("halwest gold subset policy regression passed")


if __name__ == "__main__":
    main()
