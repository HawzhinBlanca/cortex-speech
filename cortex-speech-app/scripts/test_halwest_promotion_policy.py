import tempfile
from pathlib import Path

import promote_b7876_corrections as promotion


def write_file(path: Path, content: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def approved_row(row_id: str, audio: Path) -> dict:
    return {
        "id": row_id,
        "review_status": "approved",
        "corrected_text": "corrected Sorani text",
        "review_audio_file": str(audio),
        "start_sec": "1.0",
        "duration_sec": "3.5",
    }


def test_approved_rows_only_accept_review_wavs() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        asr_dir = root / "out" / "asr_draft" / "B7876RX"
        trusted_audio = asr_dir / "review_wavs" / "clip.wav"
        external_audio = root / "outside.wav"
        non_wav = asr_dir / "review_wavs" / "clip.txt"
        write_file(trusted_audio, "wav")
        write_file(external_audio, "wav")
        write_file(non_wav, "txt")

        original_asr_dir = promotion.ASR_DIR
        try:
            promotion.ASR_DIR = asr_dir
            rows = [
                approved_row("ok", trusted_audio),
                approved_row("external", external_audio),
                approved_row("not_wav", non_wav),
                {**approved_row("blank_text", trusted_audio), "corrected_text": " "},
            ]
            approved = promotion.approved_rows(rows)
            assert [row["id"] for row in approved] == ["ok"]
            assert Path(approved[0]["review_audio_file"]).resolve() == trusted_audio.resolve()
        finally:
            promotion.ASR_DIR = original_asr_dir


def test_paths_follow_current_out_dir_without_derived_overrides() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        asr_dir = out_dir / "asr_draft" / "B7876RX"
        template = asr_dir / "B7876RX_correction_template.csv"
        report = asr_dir / "B7876RX_promotion_report.json"
        write_file(asr_dir / "review_wavs" / "clip.wav", "wav")
        template.parent.mkdir(parents=True, exist_ok=True)
        template.write_text(
            "id,review_status,corrected_text,review_audio_file,start_sec,duration_sec\n"
            f"clip,approved,text,{asr_dir / 'review_wavs' / 'clip.wav'},0,1\n",
            encoding="utf-8",
        )

        original_out = promotion.OUT_DIR
        try:
            promotion.OUT_DIR = out_dir
            assert promotion.asr_dir() == asr_dir
            assert promotion.correction_template() == template
            assert promotion.promotion_report() == report
            rows = promotion.read_rows()
            assert len(rows) == 1
        finally:
            promotion.OUT_DIR = original_out


def test_promote_skips_duplicates_and_invalid_audio() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        asr_dir = out_dir / "asr_draft" / "B7876RX"
        trusted_dir = out_dir / "tts_voice_cloning"
        trusted_audio = asr_dir / "review_wavs" / "clip.wav"
        external_audio = root / "outside.wav"
        write_file(trusted_audio, "wav")
        write_file(external_audio, "wav")

        original_out = promotion.OUT_DIR
        original_asr_dir = promotion.ASR_DIR
        original_trusted_dir = promotion.TRUSTED_DIR
        original_report = promotion.PROMOTION_REPORT
        original_finalize = promotion.finalize_dataset
        try:
            promotion.OUT_DIR = out_dir
            promotion.ASR_DIR = asr_dir
            promotion.TRUSTED_DIR = trusted_dir
            promotion.PROMOTION_REPORT = asr_dir / "promotion_report.json"
            promotion.finalize_dataset = lambda: None

            rows = [
                approved_row("clip_0001", trusted_audio),
                approved_row("clip_0001", trusted_audio),
                approved_row("external", external_audio),
                approved_row("../escape", trusted_audio),
            ]
            report = promotion.promote(rows)
            assert report["new_rows_promoted"] == 1
            assert report["skipped_duplicate_ids"] == 2
            assert report["skipped_invalid_audio"] == 1
            assert (trusted_dir / "wavs" / "clip_0001.wav").exists()
            assert not (trusted_dir / "escape.wav").exists()
            manifest_lines = (trusted_dir / "manifest.jsonl").read_text(encoding="utf-8").splitlines()
            assert len(manifest_lines) == 1
        finally:
            promotion.OUT_DIR = original_out
            promotion.ASR_DIR = original_asr_dir
            promotion.TRUSTED_DIR = original_trusted_dir
            promotion.PROMOTION_REPORT = original_report
            promotion.finalize_dataset = original_finalize


def main() -> None:
    test_approved_rows_only_accept_review_wavs()
    test_paths_follow_current_out_dir_without_derived_overrides()
    test_promote_skips_duplicates_and_invalid_audio()
    print("halwest promotion policy regression passed")


if __name__ == "__main__":
    main()
