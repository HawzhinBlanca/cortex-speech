import tempfile
from pathlib import Path

import asr_draft_b7876 as asr


def test_asr_paths_follow_current_roots_without_derived_overrides() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        source_dir = root / "source"
        out_dir = root / "out"

        original_source_dir = asr.SOURCE_DIR
        original_out_dir = asr.OUT_DIR
        try:
            asr.SOURCE_DIR = source_dir
            asr.OUT_DIR = out_dir
            assert asr.source_audio() == source_dir / "B7876RX.wav"
            assert asr.asr_dir() == out_dir / "asr_draft" / "B7876RX"
            assert asr.asr_input() == out_dir / "asr_draft" / "B7876RX" / "B7876RX_16k_asr_input.wav"
        finally:
            asr.SOURCE_DIR = original_source_dir
            asr.OUT_DIR = original_out_dir


def test_asr_explicit_path_overrides_are_honored() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        source_audio = root / "custom.wav"
        asr_dir = root / "custom_asr"
        asr_input = root / "custom_input.wav"

        original_source_audio = asr.SOURCE_AUDIO
        original_asr_dir = asr.ASR_DIR
        original_asr_input = asr.ASR_INPUT
        try:
            asr.SOURCE_AUDIO = source_audio
            asr.ASR_DIR = asr_dir
            asr.ASR_INPUT = asr_input
            assert asr.source_audio() == source_audio
            assert asr.asr_dir() == asr_dir
            assert asr.asr_input() == asr_input
        finally:
            asr.SOURCE_AUDIO = original_source_audio
            asr.ASR_DIR = original_asr_dir
            asr.ASR_INPUT = original_asr_input


def test_clean_asr_text_limits_repeated_tokens() -> None:
    assert asr.clean_asr_text("  hello   hello hello hello hello world  ") == "hello hello hello world"


def main() -> None:
    test_asr_paths_follow_current_roots_without_derived_overrides()
    test_asr_explicit_path_overrides_are_honored()
    test_clean_asr_text_limits_repeated_tokens()
    print("halwest asr draft policy regression passed")


if __name__ == "__main__":
    main()
