#!/usr/bin/env python3
"""Pins for the duplicate-content audit's pure core (the owner's 2026-08-17 find, as fixtures)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_dataset_duplicates import (  # noqa: E402
    audio_says_duplicate,
    confirm_groups_with_audio,
    duplicate_groups,
)

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


# ── RULE C: the audio decides (2026-08-18) ──────────────────────────────────────────────────────
#
# Text alone cannot tell a duplicated import from a narrator saying the same sentence twice. Measured
# on the first 5 audiobooks imported, ALL flagged groups were legitimate repeats — every episode of
# `bangewazek_bo_behesht` announces the series title, and a ghazal collection repeats verses. With
# 134 books to import the gate would have gone permanently red and been ignored.
#
# The danger of that fix is BLINDING the gate, so these pin both directions: the owner's real find
# must still fail, and unreadable audio must never be waved through.


def _reading(seconds: float, freq: float, seed: int = 0, phase: float = 0.0):
    """A stand-in for one spoken take.

    Deliberately NOT a bare sine: two phase-aligned pure tones are genuinely near-identical signals,
    so a fixture built from them would test nothing. A take carries its own phase, a wobbling pitch
    contour, and its own noise — which is exactly what makes two readings of one sentence
    decorrelate while a duplicated import stays identical.
    """
    import numpy as np

    t = np.linspace(0.0, seconds, int(16000 * seconds), endpoint=False)
    rng = np.random.default_rng(seed)
    contour = freq * (1.0 + 0.03 * np.sin(2 * np.pi * (0.7 + 0.3 * seed) * t + seed))
    return (np.sin(2 * np.pi * contour * t + phase) + 0.15 * rng.standard_normal(t.size)).astype("float32")


def test_identical_audio_is_still_a_duplicate() -> None:
    """The owner's actual find: the same recording under two names. This MUST keep failing."""
    clip = _reading(1.5, 220.0, seed=1)
    assert audio_says_duplicate(clip, clip.copy()) is True


def test_two_readings_of_one_sentence_are_not_a_duplicate() -> None:
    """A series intro read in episode 4 and again in episode 8 — same words, different audio."""
    assert audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.5, 231.0, seed=2)) is False
    # Same speaker, same words, same nominal pitch — a second take still decorrelates, because its
    # phase and contour are its own. This is the case the audiobook intros actually produce.
    assert (
        audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.5, 220.0, seed=9, phase=1.1))
        is False
    )


def test_a_different_length_reading_is_not_a_duplicate() -> None:
    """Two readings of one sentence never match to the millisecond; a duplicate does."""
    assert audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.9, 220.0, seed=1)) is False


def test_unreadable_audio_is_never_declared_clean() -> None:
    """None, not False. A clip whose audio cannot be read keeps FAILING the gate."""
    assert audio_says_duplicate(None, _reading(1.0, 220.0)) is None
    assert audio_says_duplicate(_reading(1.0, 220.0), None) is None

    # And the group-level wiring keeps it in the failing set rather than the cleared one.
    rows = [
        ("a", r"D:\gone\missing_a.wav", ALIGN, TEXT, 0),
        ("b", r"D:\gone\missing_b.wav", ALIGN, TEXT, 1),
    ]
    groups = duplicate_groups(rows)
    assert len(groups) == 1, groups
    confirmed, unconfirmed, repeats = confirm_groups_with_audio(groups, rows)
    assert not confirmed and not repeats, (confirmed, repeats)
    assert len(unconfirmed) == 1, unconfirmed


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"DATASET DUPLICATE AUDIT CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
