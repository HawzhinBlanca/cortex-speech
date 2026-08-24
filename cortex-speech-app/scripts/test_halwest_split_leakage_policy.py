"""Halwest dataset: split leakage + absolute-path policy.

Two defects this pins, both found in the 2026-08-25 audit:

1. `finalize_halwest_dataset.split_rows` used to carve ~5% of EVERY source recording into test
   and another ~5% into validation, putting the same recording (same mic, room, prosody, one
   transcript) on both sides of the split, and `DATASET_CARD.md` handed that off as "trusted"
   train/validation/test. The repo's own rule is the opposite — see
   `src-tauri/src/export.rs::assign_splits`, "No source-recording leakage".
2. The emitted manifests/CSVs carried the curator's ABSOLUTE filesystem paths, the same privacy
   leak the Rust exporters strip and `scripts/test_windows_repo_hygiene.py` blocks in tracked
   files.
"""

import re
import tempfile
from pathlib import Path

import build_halwest_dataset as builder
import create_halwest_gold_subset as gold
import finalize_halwest_dataset as finalize


ABSOLUTE_PATH = re.compile(r"^(?:[A-Za-z]:[\\/]|\\\\|/)")
PATH_KEYS = ("path", "file", "audio", "transcript", "dir")


def make_rows(source: str, count: int, duration: float = 9.0) -> list[dict]:
    return [
        {
            "id": f"{source.removesuffix('.wav')}_{idx:04d}",
            "speaker": "halwest",
            "audio_filepath": f"tts_voice_cloning/wavs/{source.removesuffix('.wav')}_{idx:04d}.wav",
            "source_audio": source,
            "duration": duration,
            "text": "x" * 100,
        }
        for idx in range(1, count + 1)
    ]


def sources_in(split: list[dict]) -> set[str]:
    return {Path(row["source_audio"]).name for row in split}


def assert_no_absolute_paths(rows: list[dict], where: str) -> None:
    for row in rows:
        for key, value in row.items():
            if not isinstance(value, str) or not any(token in key for token in PATH_KEYS):
                continue
            assert not ABSOLUTE_PATH.match(value), f"{where}: {key} leaks an absolute path: {value}"


def test_no_source_recording_appears_in_two_splits() -> None:
    rows = make_rows("A.wav", 40) + make_rows("B.wav", 30) + make_rows("C.wav", 8) + make_rows("D.wav", 6)
    splits = finalize.split_rows(rows)

    assert sum(len(split) for split in splits.values()) == len(rows)
    assert {row["id"] for split in splits.values() for row in split} == {row["id"] for row in rows}

    seen: dict[str, str] = {}
    for name, split in splits.items():
        for source in sources_in(split):
            assert source not in seen, f"{source} is in both {seen[source]} and {name} — recording leakage"
            seen[source] = name

    # With four recordings all three splits must actually be filled; empty val/test presented as
    # a real split is the exact failure export.rs documents.
    for name, split in splits.items():
        assert split, f"{name} split is empty even though four recordings exist"


def test_too_few_recordings_is_train_only() -> None:
    for source_count in (1, 2):
        rows: list[dict] = []
        for source in "AB"[:source_count]:
            rows.extend(make_rows(f"{source}.wav", 40))
        splits = finalize.split_rows(rows)
        assert len(splits["train"]) == len(rows), f"{source_count} recording(s): train must hold everything"
        assert splits["validation"] == [], f"{source_count} recording(s): validation must be empty, not contaminated"
        assert splits["test"] == [], f"{source_count} recording(s): test must be empty, not contaminated"


def test_split_is_deterministic_and_order_independent() -> None:
    rows = make_rows("A.wav", 12) + make_rows("B.wav", 9) + make_rows("C.wav", 5)
    first = finalize.split_rows(rows)
    second = finalize.split_rows(list(reversed(rows)))
    assert {name: [r["id"] for r in split] for name, split in first.items()} == {
        name: [r["id"] for r in split] for name, split in second.items()
    }


def test_dataset_card_never_calls_a_train_only_build_a_real_split() -> None:
    source = Path(finalize.__file__).read_text(encoding="utf-8")
    assert "TRAIN ONLY" in source, "the card must say plainly when there is no held-out split"
    readme = Path(finalize.__file__).read_text(encoding="utf-8")
    assert "deterministic trusted train/validation/test splits" not in readme, (
        "README must not advertise the splits as trusted"
    )


def test_dataset_relpath_strips_curator_paths() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "Halwest_Voice_Dataset"
        clip = out_dir / "tts_voice_cloning" / "wavs" / "A_0001.wav"
        assert builder.dataset_relpath(clip, out_dir) == "tts_voice_cloning/wavs/A_0001.wav"
        assert builder.dataset_relpath(root / "sources" / "A.wav", out_dir) == "A.wav"
        assert not ABSOLUTE_PATH.match(builder.dataset_relpath(clip, out_dir))
        assert not ABSOLUTE_PATH.match(builder.dataset_relpath(root / "sources" / "A.wav", out_dir))


def test_builder_emits_no_absolute_paths() -> None:
    source = Path(builder.__file__).read_text(encoding="utf-8")
    for banned in (
        '"source_audio": str(item.audio)',
        '"audio_filepath": str(full_24)',
        '"audio_filepath": str(wav_path)',
        '"audio_file": str(wav_path)',
        '"audio_48k": str(full_48)',
        '"source_dir": str(SOURCE_DIR)',
        '"output_dir": str(OUT_DIR)',
        'ref_row["reference_audio_file"] = str(dest)',
    ):
        assert banned not in source, f"dataset artifacts must not embed absolute paths: {banned}"
    assert "def dataset_relpath(" in source


def test_gold_rows_carry_no_absolute_paths() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        out_dir = root / "out"
        rows: list[dict] = []
        for source in ("A", "B", "C"):
            for row in make_rows(f"{source}.wav", 6):
                clip = out_dir / row["audio_filepath"]
                clip.parent.mkdir(parents=True, exist_ok=True)
                clip.write_bytes(b"RIFF")
                rows.append(row)

        original = (gold.OUT_DIR, gold.GOLD_DIR, gold.GOLD_REF_DIR)
        try:
            gold.OUT_DIR = out_dir
            gold.GOLD_DIR = out_dir / "tts_voice_cloning_gold"
            gold.GOLD_REF_DIR = out_dir / "voice_cloning_gold_reference"
            exact = {f"{name}.wav": f"{name}.txt.txt" for name in ("A", "B", "C")}
            gold_rows = gold.copy_gold_rows(rows, exact)
            assert len(gold_rows) == len(rows)
            assert_no_absolute_paths(gold_rows, "gold manifest")

            summary = gold.write_gold_dataset(gold_rows)
            assert summary["clip_count"] == len(rows)
            reference = gold.read_csv(gold.GOLD_REF_DIR / "reference_manifest.csv")
            assert reference, "expected reference clips for this fixture"
            assert_no_absolute_paths(reference, "gold reference manifest")

            for name in ("train", "validation", "test"):
                split = gold.read_jsonl(gold.GOLD_DIR / "splits" / f"{name}.jsonl")
                assert split, f"gold {name} split is empty even though three recordings exist"
                assert_no_absolute_paths(split, f"gold {name} split")
        finally:
            gold.OUT_DIR, gold.GOLD_DIR, gold.GOLD_REF_DIR = original


def test_transcript_pairing_refuses_ambiguous_siblings() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        source = Path(tmp)
        (source / "A.wav").write_bytes(b"RIFF")
        (source / "A.txt").write_text("trusted", encoding="utf-8")
        (source / "A.txt.bak").write_text("stale draft", encoding="utf-8")

        original = builder.SOURCE_DIR
        try:
            builder.SOURCE_DIR = source
            # A stale `A.txt.<anything>` sibling must never stand in for `A.txt`.
            items = builder.pair_sources()
            assert len(items) == 1
            assert items[0].transcript is not None
            assert items[0].transcript.name == "A.txt"
            assert items[0].transcript_status == "matched_by_basename"

            (source / "A.txt.txt").write_text("second candidate", encoding="utf-8")
            try:
                builder.pair_sources()
            except ValueError as exc:
                assert "Ambiguous transcript pairing" in str(exc)
            else:
                raise AssertionError("pair_sources silently picked one of two transcript candidates")
        finally:
            builder.SOURCE_DIR = original


def main() -> None:
    test_no_source_recording_appears_in_two_splits()
    test_too_few_recordings_is_train_only()
    test_split_is_deterministic_and_order_independent()
    test_dataset_card_never_calls_a_train_only_build_a_real_split()
    test_dataset_relpath_strips_curator_paths()
    test_builder_emits_no_absolute_paths()
    test_gold_rows_carry_no_absolute_paths()
    test_transcript_pairing_refuses_ambiguous_siblings()
    print("halwest split leakage and absolute-path policy regression passed")


if __name__ == "__main__":
    main()
