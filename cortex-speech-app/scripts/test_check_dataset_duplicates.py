#!/usr/bin/env python3
"""Pins for the duplicate audit's AUDIO confirmation across sample rates (2026-08-25).

The gate could not catch its own target. `audio_says_duplicate` divided both sample counts by a
hardcoded 16000 and a comment claimed "both clips are resampled to a common length below anyway" —
no resampling existed anywhere in the module, and `_clip_pcm` reads each clip at its SOURCE file's
native rate. The library holds both the 48 kHz masters and 16 kHz WAVs of the same material (owner
fact), so the same 3 s sentence imported from each measured 6000 "ms" apart, blew the 120 ms
duration filter, returned False, and was filed as a legitimate repeat — the exact duplicated import
this gate exists to fail on, cleared while it printed OK at baseline 0.

These pins are mixed-rate on purpose: the same-rate cases live in test_dataset_duplicates.py and
would never have seen this.
"""

from __future__ import annotations

import json
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_dataset_duplicates import (  # noqa: E402
    AUDIO_DURATION_TOLERANCE_MS,
    _clip_pcm,
    audio_says_duplicate,
    confirm_groups_with_audio,
    duplicate_groups,
)

TEXT = "ئەم ڕستەیە بەشێکی تەواوی گفتوگۆکەیە و درێژییەکەی بەسە"
ALIGN = '{"source_start_ms": 0, "source_end_ms": 1500}'


def _audio_or_skip():
    """(numpy, soundfile) or None — CI runs the policy suite with no pip install (see the sibling)."""
    try:
        import numpy
        import soundfile

        return numpy, soundfile
    except ImportError:
        return None


def _take(seconds: float, rate: int, seed: int = 0):
    """One spoken take rendered from a 48 kHz master at `rate` — the SAME recording when `seed` matches.

    A seeded band-limited noise process under a syllabic envelope, resampled from one master, so the
    48 kHz and 16 kHz renders are two samplings of one recording; another seed is another take and
    decorrelates at every lag (the v2 verdict searches lags, which is exactly what undoes the phase
    offset the old tone fixture relied on).
    """
    import numpy as np

    master_rate = 48000
    n = int(master_rate * seconds)
    noise = np.random.default_rng(seed).standard_normal(n)
    shaped = np.convolve(noise, np.ones(24) / 24.0, mode="same")
    t = np.arange(n, dtype="float64") / master_rate
    shaped *= 0.55 + 0.45 * np.sin(2 * np.pi * 4.0 * t)
    if rate != master_rate:
        shaped = np.interp(
            np.arange(int(rate * seconds), dtype="float64") * (master_rate / rate),
            np.arange(n, dtype="float64"),
            shaped,
        )
    return shaped.astype("float32")


def _write_wav(sf, path: Path, data, rate: int) -> Path:
    """Write, then wait for the bytes to actually be readable.

    This box has a measured flake where a file written and immediately read back is short or absent;
    the settle loop is cheaper than a nondeterministic gate.
    """
    sf.write(str(path), data, rate, subtype="PCM_16")
    for _ in range(40):
        try:
            if sf.info(str(path)).frames == data.size:
                return path
        except Exception:
            pass
        time.sleep(0.05)
    raise AssertionError(f"fixture {path} never settled")


def test_the_same_sentence_at_48k_and_16k_is_a_duplicate() -> None:
    """The masters-vs-WAV case the gate was blind to. Same audio, two rates, MUST confirm."""
    if _audio_or_skip() is None:
        print("    (skipped: numpy/soundfile absent — audio confirmation is optional)")
        return
    a, b = _take(1.5, 48000, seed=1), _take(1.5, 16000, seed=1)
    assert audio_says_duplicate(a, b, 48000, 16000) is True


def test_mixed_rate_durations_are_compared_in_true_milliseconds() -> None:
    """The root cause, pinned directly: equal-length clips at different rates are 0 ms apart."""
    if _audio_or_skip() is None:
        print("    (skipped: numpy/soundfile absent — audio confirmation is optional)")
        return
    a, b = _take(1.5, 48000, seed=1), _take(1.5, 16000, seed=1)
    # The old arithmetic: |72000 - 24000| / 16000 * 1000 = 3000 ms, way past the tolerance.
    assert abs(a.size - b.size) / 16000 * 1000 > AUDIO_DURATION_TOLERANCE_MS
    # v2: a longer CUT of the same recording inside the 1.5 s tolerance is the same recording at
    # either rate; a take longer than the tolerance is still refused before any decode.
    assert audio_says_duplicate(_take(1.5, 48000, seed=1), _take(1.9, 16000, seed=1), 48000, 16000) is True
    assert audio_says_duplicate(_take(1.5, 48000, seed=1), _take(3.5, 16000, seed=1), 48000, 16000) is False


def test_two_different_readings_at_mixed_rates_are_not_a_duplicate() -> None:
    """The false-positive guard must survive the fix: same length, different audio, still False."""
    if _audio_or_skip() is None:
        print("    (skipped: numpy/soundfile absent — audio confirmation is optional)")
        return
    assert audio_says_duplicate(_take(1.5, 48000, seed=1), _take(1.5, 16000, seed=2), 48000, 16000) is False
    # Same speaker, same words, same length — a second take of the series intro, recorded at the
    # other rate. A second take is its own signal at every lag.
    assert audio_says_duplicate(_take(1.5, 48000, seed=1), _take(1.5, 16000, seed=9), 48000, 16000) is False


def test_clip_pcm_carries_the_file_rate_end_to_end() -> None:
    """Whole path on real files: 48 kHz master + 16 kHz WAV of one sentence lands in `confirmed`."""
    if _audio_or_skip() is None:
        print("    (skipped: numpy/soundfile absent — audio confirmation is optional)")
        return
    _np, sf = _audio_or_skip()
    with tempfile.TemporaryDirectory() as tmp:
        d = Path(tmp)
        master = _write_wav(sf, d / "Lamofull2_00086400_A01.wav", _take(1.5, 48000, seed=3), 48000)
        wav16 = _write_wav(sf, d / "A1-0032_PODCAST-001.wav", _take(1.5, 16000, seed=3), 16000)

        assert _clip_pcm(str(master), ALIGN)[1] == 48000
        assert _clip_pcm(str(wav16), ALIGN)[1] == 16000

        rows = [
            ("master", str(master), ALIGN, TEXT, 0),
            ("wav16", str(wav16), ALIGN, TEXT, 1),
        ]
        groups = duplicate_groups(rows)
        assert len(groups) == 1, groups
        confirmed, unconfirmed, repeats = confirm_groups_with_audio(groups, rows)
        assert len(confirmed) == 1, (confirmed, unconfirmed, repeats)
        assert not repeats and not unconfirmed, (repeats, unconfirmed)


def test_a_genuine_repeat_across_rates_is_still_cleared() -> None:
    """And the audiobook intro read twice, imported at two rates, stays a legitimate repeat."""
    if _audio_or_skip() is None:
        print("    (skipped: numpy/soundfile absent — audio confirmation is optional)")
        return
    _np, sf = _audio_or_skip()
    with tempfile.TemporaryDirectory() as tmp:
        d = Path(tmp)
        take1 = _write_wav(sf, d / "book_ep04.wav", _take(1.5, 48000, seed=1), 48000)
        take2 = _write_wav(sf, d / "book_ep08.wav", _take(1.5, 16000, seed=9), 16000)
        rows = [
            ("t1", str(take1), ALIGN, TEXT, 0),
            ("t2", str(take2), ALIGN, TEXT, 1),
        ]
        groups = duplicate_groups(rows)
        assert len(groups) == 1, groups
        confirmed, unconfirmed, repeats = confirm_groups_with_audio(groups, rows)
        assert len(repeats) == 1, (confirmed, unconfirmed, repeats)


def test_unreadable_audio_is_still_never_declared_clean() -> None:
    """`_clip_pcm` now returns a tuple; a missing file must still be None, not a False verdict."""
    rows = [
        ("a", r"D:\gone\missing_a.wav", ALIGN, TEXT, 0),
        ("b", r"D:\gone\missing_b.wav", ALIGN, TEXT, 1),
    ]
    assert _clip_pcm(rows[0][1], ALIGN) is None
    assert json.loads(ALIGN)["source_end_ms"] == 1500
    confirmed, unconfirmed, repeats = confirm_groups_with_audio(duplicate_groups(rows), rows)
    assert not confirmed and not repeats, (confirmed, repeats)
    assert len(unconfirmed) == 1, unconfirmed


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"DUPLICATE AUDIT MIXED-RATE PINS: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
