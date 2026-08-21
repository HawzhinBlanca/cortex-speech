#!/usr/bin/env python3
"""Regression tests for the live hidden-check capacity gate."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_spot_check_pool import (  # noqa: E402
    available_keys_by_reviewer,
    learning_key,
    required_keys_for_work,
    serving_constants,
    work_counts_by_reviewer,
)


def test_learning_key_matches_the_rust_whitespace_and_case_contract() -> None:
    assert learning_key("  Hello\t WORLD ") == "hello world"
    assert learning_key("دەقی   ڕاست") == learning_key("دەقی ڕاست")


def test_capacity_is_per_reviewer_focus_dialect_and_prior_score() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        sorani = root / "Kurdish Corpora" / "sorani" / "ZarPodcast" / "clip.wav"
        hawleri = root / "KBHP-EP01.wav"
        sorani.parent.mkdir(parents=True)
        sorani.write_bytes(b"RIFF")
        hawleri.write_bytes(b"RIFF")
        candidates = [
            ("sor", str(sorani), "draft one", "correct one"),
            ("haw", str(hawleri), "draft two", "correct two"),
            # A draft already correct under the Rust learning key is not a listening check.
            ("noop", str(sorani), " SAME   TEXT ", "same text"),
        ]
        roster = {"Roza": ["sorani"], "Hawzhin": ["sorani", "hawleri"]}
        table = [("Kurdish Corpora\\sorani\\", "sorani"), ("KBHP", "hawleri")]

        focused = available_keys_by_reviewer(
            reviewers=["Roza", "Hawzhin"],
            roster=roster,
            focus={"haw"},
            candidates=candidates,
            already_scored=set(),
            dialect_table=table,
        )
        assert focused == {"Roza": 0, "Hawzhin": 1}

        scored = available_keys_by_reviewer(
            reviewers=["Hawzhin"],
            roster=roster,
            focus={"haw"},
            candidates=candidates,
            already_scored={("haw", "hawzhin")},
            dialect_table=table,
        )
        assert scored == {"Hawzhin": 0}, "a previously answered key must not count as fresh capacity"


def test_serving_constants_are_read_from_the_rust_source() -> None:
    source = "const QUEUE_BATCH: usize = 25;\nconst SPOT_CHECK_EVERY: usize = 8;"
    assert serving_constants(source) == (25, 8)


def test_missing_or_invalid_serving_constants_fail_closed() -> None:
    sources = [
        "",
        "const QUEUE_BATCH: usize = 25;",
        "const QUEUE_BATCH: usize = 0;\nconst SPOT_CHECK_EVERY: usize = 8;",
    ]
    for source in sources:
        try:
            serving_constants(source)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"broken serving policy was accepted: {source!r}")


def test_capacity_matches_per_refill_rounding() -> None:
    expected = {
        0: 0,
        1: 1,
        8: 1,
        9: 2,
        25: 4,
        26: 5,
        1_293: 207,
    }
    assert {work: required_keys_for_work(work, 25, 8) for work in expected} == expected


def test_capacity_rejects_nonsensical_inputs() -> None:
    for args in [(-1, 25, 8), (1, 0, 8), (1, 25, 0)]:
        try:
            required_keys_for_work(*args)
        except ValueError:
            pass
        else:
            raise AssertionError(f"invalid capacity inputs were accepted: {args}")


def test_work_capacity_is_per_reviewer_dialect() -> None:
    clips = [(r"D:\KBHP-EP01.wav", 1), (r"D:\Kurdish Corpora\sorani\ZarPodcast\z.wav", 1)]
    roster = {"Roza": ["sorani"], "Hawzhin": ["sorani", "hawleri"]}
    table = [("Kurdish Corpora\\sorani\\", "sorani"), ("KBHP", "hawleri")]
    assert work_counts_by_reviewer(
        reviewers=["Roza", "Hawzhin"],
        roster=roster,
        clips=clips,
        dialect_table=table,
    ) == {"Roza": 1, "Hawzhin": 2}


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"SPOT-CHECK GATE CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
