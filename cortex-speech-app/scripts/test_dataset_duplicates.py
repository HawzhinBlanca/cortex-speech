#!/usr/bin/env python3
"""Pins for the duplicate-content audit's pure core (the owner's 2026-08-17 find, as fixtures)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_dataset_duplicates import duplicate_groups  # noqa: E402

TEXT = "ئەم ڕستەیە بەشێکی تەواوی گفتوگۆکەیە و درێژییەکەی بەسە"
ALIGN = '{"source_start_ms": 132945, "source_end_ms": 140740}'


def test_the_real_find_same_offset_and_text_in_two_files_is_one_group() -> None:
    rows = [
        ("a", r"D:\x\Lamofull2_00086400_A01.flac", ALIGN, TEXT, 0),
        ("b", r"D:\x\Lamofull00086400_A02.flac", ALIGN, TEXT, 1),
    ]
    groups = duplicate_groups(rows)
    assert len(groups) == 1 and len(groups[0]) == 2, groups


def test_the_same_sentence_within_one_file_is_not_a_duplicate() -> None:
    # Clips from ONE file are the recording itself, not a re-import of it.
    rows = [
        ("a", r"D:\x\ep1.wav", ALIGN, TEXT, 0),
        ("b", r"D:\x\ep1.wav", ALIGN, TEXT, 0),
    ]
    assert duplicate_groups(rows) == []


def test_a_repeated_short_phrase_in_different_recordings_is_not_flagged() -> None:
    # "بەڵێ" recurs everywhere by chance; only offset+LONG-text agreement means the same recording.
    rows = [
        ("a", r"D:\x\ep1.wav", ALIGN, "بەڵێ", 0),
        ("b", r"D:\x\ep2.wav", ALIGN, "بەڵێ", 0),
    ]
    assert duplicate_groups(rows) == []


def test_same_text_at_a_different_timeline_position_is_not_flagged() -> None:
    # A genuinely repeated long sentence in two different recordings does not sit at the same ms.
    other = '{"source_start_ms": 901000, "source_end_ms": 908000}'
    rows = [
        ("a", r"D:\x\ep1.wav", ALIGN, TEXT, 0),
        ("b", r"D:\x\ep2.wav", other, TEXT, 0),
    ]
    assert duplicate_groups(rows) == []


def test_offsets_within_the_encoder_padding_bucket_still_match() -> None:
    # The real pair 783adbcd/ad2cf706 differed by 13 ms (encoder padding); the bucket absorbs it.
    near = '{"source_start_ms": 133245, "source_end_ms": 141040}'
    rows = [
        ("a", r"D:\x\one.flac", ALIGN, TEXT, 0),
        ("b", r"D:\x\two.flac", near, TEXT, 0),
    ]
    assert len(duplicate_groups(rows)) == 1


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"DATASET DUPLICATE AUDIT CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
