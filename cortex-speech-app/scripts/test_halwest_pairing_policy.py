import tempfile
from pathlib import Path

import build_halwest_dataset as builder


def write_file(path: Path, content: str = "") -> None:
    path.write_text(content, encoding="utf-8")


def pair_map(items):
    return {item.audio.name: item for item in items}


def test_pairing_policy() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        source = Path(tmp)
        for name in ["B7890RX.wav", "Halwest1.wav"]:
            write_file(source / name)
        write_file(source / "B7890RX.txt.txt")
        write_file(source / "B8790RX.txt.txt", "candidate transcript")
        write_file(source / "Halwest1.txt.txt", "confirmed transcript")

        original_source = builder.SOURCE_DIR
        original_allow = builder.ALLOW_INFERRED_TRANSCRIPT_PAIRS
        try:
            builder.SOURCE_DIR = source
            builder.ALLOW_INFERRED_TRANSCRIPT_PAIRS = False
            default_items = pair_map(builder.pair_sources())
            assert default_items["B7890RX.wav"].transcript_status == "transcript_file_empty"
            assert not builder.training_trusted(default_items["B7890RX.wav"])
            assert builder.training_trusted(default_items["Halwest1.wav"])

            builder.ALLOW_INFERRED_TRANSCRIPT_PAIRS = True
            inferred_items = pair_map(builder.pair_sources())
            assert (
                inferred_items["B7890RX.wav"].transcript_status
                == "inferred_B8790RX_transcript_explicitly_allowed_review_only"
            )
            assert not builder.training_trusted(inferred_items["B7890RX.wav"])
        finally:
            builder.SOURCE_DIR = original_source
            builder.ALLOW_INFERRED_TRANSCRIPT_PAIRS = original_allow


def test_prepare_output_dir_safety() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        source = root / "source"
        output = root / "Halwest_Voice_Dataset"
        source.mkdir()
        (output / "asr_draft" / "B7876RX").mkdir(parents=True)
        write_file(output / "asr_draft" / "B7876RX" / "draft.txt", "keep")
        write_file(output / "old_file.txt", "delete")

        original_source = builder.SOURCE_DIR
        original_out = builder.OUT_DIR
        try:
            builder.SOURCE_DIR = source
            builder.OUT_DIR = output
            builder.prepare_output_dir()
            assert output.exists()
            assert not (output / "old_file.txt").exists()
            assert (output / "asr_draft" / "B7876RX" / "draft.txt").read_text(encoding="utf-8") == "keep"

            builder.OUT_DIR = source / "nested_output"
            try:
                builder.prepare_output_dir()
            except ValueError as exc:
                assert "inside source directory" in str(exc)
            else:
                raise AssertionError("prepare_output_dir accepted an output path inside source")
        finally:
            builder.SOURCE_DIR = original_source
            builder.OUT_DIR = original_out


def main() -> None:
    test_pairing_policy()
    test_prepare_output_dir_safety()
    print("halwest pairing policy regression passed")


if __name__ == "__main__":
    main()
