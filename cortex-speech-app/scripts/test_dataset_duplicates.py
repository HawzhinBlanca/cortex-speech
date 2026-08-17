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


def test_the_mp4_lesson_exact_text_is_a_duplicate_at_ANY_offset() -> None:
    # The first version required offset agreement and the owner then heard the same sentence AGAIN:
    # A1-0032_PODCAST-001.mp4 is a third encode whose clock is shifted by a constant 137.8 s. An
    # identical >= 25-char decode in two files IS the same recording — real decodes of genuinely
    # different recordings always drift somewhere in a sentence that long.
    shifted = '{"source_start_ms": 141725, "source_end_ms": 149640}'
    rows = [
        ("flac", r"D:\x\Lamofull2_00086400_A01.flac", ALIGN, TEXT, 1),
        ("mp4", r"D:\x\A1-0032_PODCAST-001.mp4", shifted, TEXT, 1),
    ]
    groups = duplicate_groups(rows)
    assert len(groups) == 1 and len(groups[0]) == 2, groups


def test_drifted_text_at_the_same_offset_is_a_duplicate() -> None:
    # The 783adbcd/ad2cf706 pair: same clock position, decodes one letter apart (بەڵێ/بەلێ).
    drifted = TEXT.replace("تەواوی", "تەڵاوی")
    near = '{"source_start_ms": 132958, "source_end_ms": 140753}'
    rows = [
        ("a", r"D:\x\one.flac", ALIGN, TEXT, 0),
        ("b", r"D:\x\two.flac", near, drifted, 0),
    ]
    groups = duplicate_groups(rows)
    assert len(groups) == 1 and len(groups[0]) == 2, groups


def test_different_text_at_the_same_offset_is_not_flagged() -> None:
    # Two recordings can coincidentally place DIFFERENT sentences near the same clock position;
    # rule B needs >= 90% text similarity, not just the offset.
    other_text = "ئەمە ڕستەیەکی تەواو جیاوازە و هیچ پەیوەندییەکی بەوی ترەوە نییە"
    rows = [
        ("a", r"D:\x\one.flac", ALIGN, TEXT, 0),
        ("b", r"D:\x\two.flac", ALIGN, other_text, 0),
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
