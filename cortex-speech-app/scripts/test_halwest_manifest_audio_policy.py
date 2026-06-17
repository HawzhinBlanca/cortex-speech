import tempfile
import wave
from pathlib import Path

import audio_qc_halwest as audio_qc
import finalize_halwest_dataset as finalize


def write_file(path: Path, content: bytes = b"data") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)


def write_wav(path: Path, sample_rate: int = 24000, frames: int = 2400) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        wav.writeframes(b"\x00\x00" * frames)


def expect_error(fn, expected: str) -> None:
    try:
        fn()
    except Exception as exc:
        assert expected in str(exc)
    else:
        raise AssertionError(f"Expected error containing {expected!r}")


def test_manifest_audio_paths_are_dataset_scoped() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "clip.wav"
        external = root / "external.wav"
        write_file(audio)
        write_file(external)

        original_out = finalize.OUT_DIR
        try:
            finalize.OUT_DIR = out_dir
            assert finalize.checked_manifest_audio_path({"id": "ok", "audio_filepath": "tts_voice_cloning/wavs/clip.wav"}) == audio.resolve()
            assert finalize.checked_manifest_audio_path({"id": "ok_abs", "audio_filepath": str(audio)}) == audio.resolve()
            expect_error(
                lambda: finalize.checked_manifest_audio_path({"id": "external", "audio_filepath": str(external)}),
                "outside dataset output",
            )
            expect_error(
                lambda: finalize.checked_manifest_audio_path({"id": "missing", "audio_filepath": "tts_voice_cloning/wavs/missing.wav"}),
                "not found",
            )
            expect_error(
                lambda: finalize.checked_manifest_audio_path({"id": "blank", "audio_filepath": ""}),
                "Missing audio_filepath",
            )
        finally:
            finalize.OUT_DIR = original_out


def test_audio_qc_reports_outside_manifest_paths_as_issues() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        external = root / "external.wav"
        write_file(external)

        row = {"id": "external", "audio_filepath": str(external), "duration": "1.0"}
        original_out = finalize.OUT_DIR
        try:
            finalize.OUT_DIR = out_dir
            qc_row = audio_qc.qc_manifest_row(row)
            assert "sample_rate" not in qc_row
            assert "outside dataset output" in qc_row["issues"]
        finally:
            finalize.OUT_DIR = original_out


def test_finalize_main_uses_current_out_dir() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "clip.wav"
        write_file(audio)
        manifest = out_dir / "tts_voice_cloning" / "manifest.jsonl"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            '{"id":"clip","speaker":"halwest","audio_filepath":"tts_voice_cloning/wavs/clip.wav","source_audio":"Halwest1.wav","duration":1.0,"text":"trusted text"}\n',
            encoding="utf-8",
        )

        original_out = finalize.OUT_DIR
        try:
            finalize.OUT_DIR = out_dir
            finalize.main()
            assert (out_dir / "tts_voice_cloning" / "splits" / "train.jsonl").exists()
            assert (out_dir / "quality_control" / "trusted_tts_quality_summary.json").exists()
            assert not (root / "quality_control" / "trusted_tts_quality_summary.json").exists()
        finally:
            finalize.OUT_DIR = original_out


def test_audio_qc_main_uses_current_out_dir() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "clip.wav"
        write_wav(audio)
        manifest = out_dir / "tts_voice_cloning" / "manifest.jsonl"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            '{"id":"clip","audio_filepath":"tts_voice_cloning/wavs/clip.wav","duration":0.1}\n',
            encoding="utf-8",
        )

        original_out = audio_qc.OUT_DIR
        try:
            audio_qc.OUT_DIR = out_dir
            audio_qc.main()
            assert (out_dir / "quality_control" / "trusted_tts_audio_qc.csv").exists()
            assert (out_dir / "quality_control" / "trusted_tts_audio_qc_summary.json").exists()
            assert not (root / "quality_control" / "trusted_tts_audio_qc.csv").exists()
        finally:
            audio_qc.OUT_DIR = original_out


def main() -> None:
    test_manifest_audio_paths_are_dataset_scoped()
    test_audio_qc_reports_outside_manifest_paths_as_issues()
    test_finalize_main_uses_current_out_dir()
    test_audio_qc_main_uses_current_out_dir()
    print("halwest manifest audio policy regression passed")


if __name__ == "__main__":
    main()
