#!/usr/bin/env python3
"""Pins for the duplicate-content audit's pure core (the owner's 2026-08-17 find, as fixtures)."""

from __future__ import annotations

import sys
import difflib
import random
import sqlite3
from unittest import mock
from pathlib import Path, PureWindowsPath

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_dataset_duplicates import (  # noqa: E402
    audio_says_duplicate,
    confirm_groups_with_audio,
    duplicate_groups,
    load_audit_rows,
)

TEXT = "ئەم ڕستەیە بەشێکی تەواوی گفتوگۆکەیە و درێژییەکەی بەسە"
ALIGN = '{"source_start_ms": 132945, "source_end_ms": 140740}'


def _pool_scope_database() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        """
        CREATE TABLE speech_segments(
            id TEXT PRIMARY KEY, audio_path TEXT, alignment_json TEXT,
            raw_transcript TEXT, verified INTEGER
        );
        CREATE TABLE review_pool_registry(
            singleton_key INTEGER PRIMARY KEY, pool_id TEXT, focus_segment_count INTEGER
        );
        CREATE TABLE review_pool_members(
            pool_id TEXT, segment_id TEXT, raw_transcript TEXT,
            source_start_ms INTEGER, source_end_ms INTEGER
        );
        CREATE TABLE review_pool_dedup_manifests(
            pool_id TEXT, source_focus_segment_count INTEGER, canonical_count INTEGER,
            excluded_count INTEGER, unconfirmed_risk_count INTEGER
        );
        CREATE TABLE review_pool_duplicate_exclusions(pool_id TEXT, segment_id TEXT);
        INSERT INTO review_pool_registry VALUES(1, 'pool', 3);
        INSERT INTO speech_segments VALUES
            ('canonical', 'one.wav', '{}', 'same', 0),
            ('excluded', 'two.wav', '{}', 'same', 0),
            ('unique', 'three.wav', '{}', 'different', 0);
        INSERT INTO review_pool_members VALUES
            ('pool', 'canonical', 'same', 0, 1000),
            ('pool', 'excluded', 'same', 0, 1000),
            ('pool', 'unique', 'different', 1000, 2000);
        INSERT INTO review_pool_dedup_manifests VALUES('pool', 3, 2, 1, 0);
        INSERT INTO review_pool_duplicate_exclusions VALUES('pool', 'excluded');
        """
    )
    return connection


def test_active_pool_duplicate_audit_uses_only_verified_canonical_overlay() -> None:
    connection = _pool_scope_database()
    rows, scope = load_audit_rows(connection)
    connection.close()
    assert [row[0] for row in rows] == ["canonical", "unique"]
    assert "canonical review pool" in scope and "2 clips" in scope


def test_active_pool_duplicate_audit_fails_closed_on_exclusion_count_drift() -> None:
    connection = _pool_scope_database()
    connection.execute("DELETE FROM review_pool_duplicate_exclusions")
    try:
        load_audit_rows(connection)
    except ValueError as error:
        assert "inconsistent dedup authority" in str(error)
    else:
        raise AssertionError("dedup exclusion drift was accepted")
    finally:
        connection.close()


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


def test_transitive_offset_cluster_does_not_compare_pairs_more_than_500ms_apart() -> None:
    # 0 -> 400 -> 800 ms is one connected cluster, but the endpoints are not at the same source
    # position. The old all-pairs flush compared them anyway and could manufacture a false duplicate.
    near_but_not_exact = TEXT.replace("درێژییەکەی", "درێژییەکی")
    unrelated = "ئەم دەقە ناوەڕۆکێکی جیاوازی هەیە و تەنها بۆ پڕکردنەوەی تاقیکردنەوەکەیە"
    rows = [
        ("a", r"D:\x\one.flac", '{"source_start_ms": 0}', TEXT, 0),
        ("bridge", r"D:\x\bridge.flac", '{"source_start_ms": 400}', unrelated, 0),
        ("c", r"D:\x\three.flac", '{"source_start_ms": 800}', near_but_not_exact, 0),
    ]
    assert duplicate_groups(rows) == []


def test_vectorized_rule_b_keeps_the_exact_matcher_semantics() -> None:
    try:
        import numpy  # noqa: F401
    except ImportError:
        print("    (skipped: numpy absent — large live clusters fail closed rather than scan forever)")
        return

    drifted = TEXT.replace("تەواوی", "تەڵاوی")
    different = "ئەمە دەقێکی جیاواز و درێژە کە نابێت وەک دووبارە ناسێنرێت لە تاقیکردنەوەدا"
    # Forward separators, unlike the Windows literals elsewhere in this file: duplicate_groups
    # identifies clips with os.path.basename, which is platform-flavoured, so r"D:\x\one.flac" keeps
    # its whole string as the "basename" off Windows and the hard-coded expectation below fails for
    # a path reason rather than a duplicate-detection one. ntpath and posixpath BOTH reduce
    # "/x/one.flac" to "one.flac", so the assertion stays literal and exact on every platform.
    # Masked until now only because this case is numpy-gated and CI pip-installs nothing.
    rows = [
        ("a", "/x/one.flac", ALIGN, TEXT, 0),
        ("b", "/x/two.flac", ALIGN, drifted, 0),
        ("c", "/x/three.flac", ALIGN, different, 0),
    ]
    with mock.patch("check_dataset_duplicates.RULE_B_VECTOR_THRESHOLD", 2):
        groups = duplicate_groups(rows)
    assert groups == [[("a", "one.flac"), ("b", "two.flac")]], groups


def test_vectorized_prefilters_never_drop_a_90_percent_sequence_match() -> None:
    try:
        import numpy  # noqa: F401
    except ImportError:
        print("    (skipped: numpy absent — large live clusters fail closed rather than scan forever)")
        return

    rng = random.Random(20260824)
    alphabet = "ابتپجچحخدرڕزژسشعغفقکگلمنهوەیێ "
    rows = []
    expected = set()
    for index in range(40):
        original = "".join(rng.choice(alphabet) for _ in range(rng.randint(40, 90)))
        changed = list(original)
        # Two substitutions keep these fixtures safely above the production 90% predicate while
        # exercising character and four-gram bounds rather than exact-text Rule A.
        for position in rng.sample(range(len(changed)), 2):
            replacement = rng.choice(alphabet)
            while replacement == changed[position]:
                replacement = rng.choice(alphabet)
            changed[position] = replacement
        changed = "".join(changed)
        assert difflib.SequenceMatcher(None, original, changed).ratio() >= 0.90
        left, right = f"left-{index}", f"right-{index}"
        rows.extend(
            [
                (left, rf"D:\x\left-{index}.wav", ALIGN, original, 0),
                (right, rf"D:\x\right-{index}.wav", ALIGN, changed, 0),
            ]
        )
        expected.add(frozenset((left, right)))

    with mock.patch("check_dataset_duplicates.RULE_B_VECTOR_THRESHOLD", 2):
        groups = duplicate_groups(rows)
    actual_pairs = {
        frozenset((group[i][0], group[j][0]))
        for group in groups
        for i in range(len(group))
        for j in range(i + 1, len(group))
    }
    assert expected <= actual_pairs, (len(expected), len(actual_pairs))


# ── RULE C: the audio decides (2026-08-18) ──────────────────────────────────────────────────────
#
# Text alone cannot tell a duplicated import from a narrator saying the same sentence twice. Measured
# on the first 5 audiobooks imported, ALL flagged groups were legitimate repeats — every episode of
# `bangewazek_bo_behesht` announces the series title, and a ghazal collection repeats verses. With
# 134 books to import the gate would have gone permanently red and been ignored.
#
# The danger of that fix is BLINDING the gate, so these pin both directions: the owner's real find
# must still fail, and unreadable audio must never be waved through.


def _numpy_or_skip():
    """numpy, or None when it is absent.

    CI runs the policy suite on a bare `setup-python` with NO pip install, so numpy and soundfile are
    not there. The audio confirmation degrades correctly without them — `audio_says_duplicate`
    returns None, every text-matched group stays UNCONFIRMED, and the gate keeps failing on it — so
    the pure-python rules are still fully pinned below. These audio-only assertions skip rather than
    error, which is the difference between "not measured here" and a red build for a missing
    optional dependency.
    """
    try:
        import numpy  # noqa: F401

        return numpy
    except ImportError:
        return None


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
    if _numpy_or_skip() is None:
        print("    (skipped: numpy absent — audio confirmation is optional, the text rules above are not)")
        return
    clip = _reading(1.5, 220.0, seed=1)
    assert audio_says_duplicate(clip, clip.copy()) is True


def test_two_readings_of_one_sentence_are_not_a_duplicate() -> None:
    """A series intro read in episode 4 and again in episode 8 — same words, different audio."""
    if _numpy_or_skip() is None:
        print("    (skipped: numpy absent — audio confirmation is optional, the text rules above are not)")
        return
    assert audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.5, 231.0, seed=2)) is False
    # Same speaker, same words, same nominal pitch — a second take still decorrelates, because its
    # phase and contour are its own. This is the case the audiobook intros actually produce.
    assert (
        audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.5, 220.0, seed=9, phase=1.1))
        is False
    )


def test_a_different_length_reading_is_not_a_duplicate() -> None:
    """Two readings of one sentence never match to the millisecond; a duplicate does."""
    if _numpy_or_skip() is None:
        print("    (skipped: numpy absent — audio confirmation is optional, the text rules above are not)")
        return
    assert audio_says_duplicate(_reading(1.5, 220.0, seed=1), _reading(1.9, 220.0, seed=1)) is False


def test_unreadable_audio_is_never_declared_clean() -> None:
    """None, not False. A clip whose audio cannot be read keeps FAILING the gate."""
    if _numpy_or_skip() is not None:
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


def test_audio_confirmation_ignores_same_file_pairs_and_splits_true_components() -> None:
    rows = [
        ("a1", r"D:\x\one.wav", ALIGN, TEXT, 0),
        ("a2", r"D:\x\one.wav", ALIGN, TEXT, 0),
        ("b", r"D:\x\two.wav", ALIGN, TEXT, 0),
        ("c", r"D:\x\three.wav", ALIGN, TEXT, 0),
    ]
    group = [[("a1", "one.wav"), ("a2", "one.wav"), ("b", "two.wav"), ("c", "three.wav")]]

    def verdict(left, right, left_rate, right_rate):
        assert left_rate == right_rate == 16_000
        names = frozenset((left, right))
        if names == frozenset(("one.wav", "two.wav")):
            return True
        return False

    # PureWindowsPath, not Path: the fixture paths above are Windows literals, and on Linux/macOS
    # `Path(r"D:\x\one.wav").name` is the WHOLE string — the stand-in clips would then all carry
    # distinct identities, `verdict` would never fire, and this pin would report "no duplicates"
    # on exactly the case it exists to catch.
    with mock.patch(
        "check_dataset_duplicates._clip_pcm",
        side_effect=lambda path, _: (PureWindowsPath(path).name, 16_000),
    ):
        with mock.patch("check_dataset_duplicates.audio_says_duplicate", side_effect=verdict) as audio:
            confirmed, unconfirmed, repeats = confirm_groups_with_audio(group, rows)
    assert confirmed == [[("a1", "one.wav"), ("a2", "one.wav"), ("b", "two.wav")]], confirmed
    assert not unconfirmed and not repeats
    assert audio.call_count == 5  # six total pairs minus the one same-file pair


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"DATASET DUPLICATE AUDIT CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
