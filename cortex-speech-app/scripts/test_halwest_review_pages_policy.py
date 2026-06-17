import tempfile
from pathlib import Path

import create_halwest_review_pages as pages


def write_file(path: Path, content: str = "wav") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def expect_error(fn, expected: str) -> None:
    try:
        fn()
    except Exception as exc:
        assert expected in str(exc)
    else:
        raise AssertionError(f"Expected error containing {expected!r}")


def test_review_audio_paths_must_stay_under_dataset_output() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "clip.wav"
        external = root / "external.wav"
        write_file(audio)
        write_file(external)

        original_out = pages.OUT_DIR
        try:
            pages.OUT_DIR = out_dir
            html = pages.render_rows(
                "Trusted",
                [{"id": "clip", "audio_filepath": "tts_voice_cloning/wavs/clip.wav", "text": "ok"}],
                "text",
                "audio_filepath",
            )
            assert "file:///" in html
            assert "clip.wav" in html

            expect_error(
                lambda: pages.render_rows(
                    "Trusted",
                    [{"id": "external", "audio_filepath": str(external), "text": "bad"}],
                    "text",
                    "audio_filepath",
                ),
                "outside dataset output",
            )
            expect_error(
                lambda: pages.render_rows(
                    "Trusted",
                    [{"id": "missing", "audio_filepath": "tts_voice_cloning/wavs/missing.wav", "text": "bad"}],
                    "text",
                    "audio_filepath",
                ),
                "not found",
            )
            expect_error(
                lambda: pages.render_rows(
                    "Trusted",
                    [{"id": "blank", "audio_filepath": "", "text": "bad"}],
                    "text",
                    "audio_filepath",
                ),
                "Missing audio path",
            )
        finally:
            pages.OUT_DIR = original_out


def test_correction_page_rejects_external_review_audio() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        external = root / "external.wav"
        write_file(external)

        original_out = pages.OUT_DIR
        try:
            pages.OUT_DIR = out_dir
            expect_error(
                lambda: pages.render_correction_page(
                    [
                        {
                            "id": "bad",
                            "review_audio_file": str(external),
                            "draft_text": "draft",
                            "corrected_text": "",
                        }
                    ]
                ),
                "outside dataset output",
            )
        finally:
            pages.OUT_DIR = original_out


def test_main_allows_missing_optional_b7876_correction_package() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        out_dir = Path(tmp) / "out"
        audio = out_dir / "tts_voice_cloning" / "wavs" / "clip.wav"
        write_file(audio)
        manifest = out_dir / "tts_voice_cloning" / "manifest.jsonl"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            '{"id":"clip","audio_filepath":"tts_voice_cloning/wavs/clip.wav","text":"ok","duration":1.0}\n',
            encoding="utf-8",
        )
        review = out_dir / "review" / "segment_review.csv"
        review.parent.mkdir(parents=True, exist_ok=True)
        review.write_text("id,audio_file,text,status,issues\n", encoding="utf-8")

        original_out = pages.OUT_DIR
        try:
            pages.OUT_DIR = out_dir
            pages.main()
            index = (out_dir / "review_pages" / "index.html").read_text(encoding="utf-8")
            assert (out_dir / "review_pages" / "trusted_tts_review.html").exists()
            assert (out_dir / "review_pages" / "needs_review_segments.html").exists()
            assert "b7876_correction_review.html" not in index
        finally:
            pages.OUT_DIR = original_out


def main() -> None:
    test_review_audio_paths_must_stay_under_dataset_output()
    test_correction_page_rejects_external_review_audio()
    test_main_allows_missing_optional_b7876_correction_package()
    print("halwest review page policy regression passed")


if __name__ == "__main__":
    main()
