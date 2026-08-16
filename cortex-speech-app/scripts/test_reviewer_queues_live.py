#!/usr/bin/env python3
"""Unit tests for the reviewer-queue gate's pure decision core.

The gate itself reads the live machine; these pin the logic that decides, given a roster and a set of
clips, whether anybody is about to open a dead link. Both incidents of 2026-08-17 appear here as
fixtures, so a regression of either fails here rather than on a reviewer's phone.
"""

from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_reviewer_queues_live import (  # noqa: E402
    dialect_of,
    evaluate_queues,
    load_roster,
    may_judge,
    source_dialects,
)

# Plain strings, not r"" — a Python raw string may not END in a backslash, and these fragments all do
# (the trailing separator is what keeps `sorani-hawleri\` from matching the `sorani\` entry).
TABLE = [
    ("KBHP", "hawleri"),
    ("Kurdish Corpora\\sorani-hawleri\\", "hawleri"),
    ("Kurdish Corpora\\sorani\\", "sorani"),
    ("Kurdish Corpora\\badini\\", "badini"),
]

HAWLERI_CLIP = (r"D:\Hawzhin Kurdish Datasets 2026\_batch_remaining\KBHP-EP01_0007.wav", 9000)
SORANI_CLIP = (r"D:\Kurdish Corpora\sorani\ZarPodcast\2\A1-0050.wav", 7000)


def test_the_real_incident_a_sorani_only_reviewer_with_no_sorani_clips_fails() -> None:
    problems, _ = evaluate_queues(
        reviewers=["Roza", "Rubar"],
        roster={"Roza": ["sorani"], "Rubar": ["hawleri", "sorani"]},
        clips=[HAWLERI_CLIP],
        table=TABLE,
    )
    assert len(problems) == 1, problems
    assert "Roza" in problems[0]
    assert "ZERO" in problems[0]


def test_once_the_sorani_corpus_is_mapped_everybody_has_work() -> None:
    problems, _ = evaluate_queues(
        reviewers=["Roza", "Rubar"],
        roster={"Roza": ["sorani"], "Rubar": ["hawleri", "sorani"]},
        clips=[HAWLERI_CLIP, SORANI_CLIP],
        table=TABLE,
    )
    assert problems == []


def test_an_unrestricted_reviewer_is_never_reported_while_any_work_exists() -> None:
    problems, _ = evaluate_queues(reviewers=["Owner"], roster={}, clips=[HAWLERI_CLIP], table=TABLE)
    assert problems == []


def test_an_empty_library_fails_for_everyone_because_nobody_can_work() -> None:
    problems, _ = evaluate_queues(reviewers=["Roza"], roster={"Roza": ["sorani"]}, clips=[], table=TABLE)
    assert len(problems) == 1


def test_a_thin_queue_warns_without_failing() -> None:
    problems, warnings = evaluate_queues(
        reviewers=["Roza"],
        roster={"Roza": ["sorani"]},
        clips=[SORANI_CLIP] * 3,
        table=TABLE,
        warn_below=10,
    )
    assert problems == []
    assert len(warnings) == 1 and "only 3 clips" in warnings[0]


def test_unmapped_clips_fail_closed_and_are_not_counted_as_work() -> None:
    # The exact shape of the incident: rows exist, audio exists, but no dialect claims them, so a
    # restricted reviewer must be reported as having nothing rather than handed an unknown dialect.
    unmapped = (r"D:\somewhere\new_corpus\ep1.wav", 5000)
    assert dialect_of(unmapped[0], TABLE) is None
    assert not may_judge(["sorani"], unmapped[0], TABLE)
    problems, _ = evaluate_queues(
        reviewers=["Roza"], roster={"Roza": ["sorani"]}, clips=[unmapped], table=TABLE
    )
    assert len(problems) == 1


def test_sorani_hawleri_never_matches_the_sorani_folder() -> None:
    assert dialect_of(r"D:\Kurdish Corpora\sorani-hawleri\KBHP\ep1.wav", TABLE) == "hawleri"
    assert dialect_of(r"D:\Kurdish Corpora\sorani\ZarPodcast\a.wav", TABLE) == "sorani"
    assert dialect_of("D:/Kurdish Corpora/sorani/ZarPodcast/a.wav", TABLE) == "sorani"


def test_a_comment_key_does_not_empty_the_roster() -> None:
    # The second half of the incident: a helpful "_comment" string made a strict parse fail, and the
    # failure path is "unrestricted" — so the whole protection switched off silently.
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text(
            json.dumps({"_comment": "how to edit", "Roza": ["sorani"]}), encoding="utf-8"
        )
        roster = load_roster(Path(tmp))
    assert roster == {"Roza": ["sorani"]}, roster


def test_the_gate_reads_the_dialect_map_out_of_the_rust_rather_than_restating_it() -> None:
    # The drift between the map and reality IS the bug, so the gate must not carry its own copy.
    dialect_rs = (Path(__file__).resolve().parent.parent / "src-tauri" / "src" / "dialect.rs").read_text(
        encoding="utf-8"
    )
    table = source_dialects(dialect_rs)
    fragments = [f for f, _ in table]
    assert ("KBHP", "hawleri") in table
    assert any("sorani" in f for f in fragments), fragments
    # Every live corpus location must resolve, or restricted reviewers silently lose that corpus.
    assert dialect_of(HAWLERI_CLIP[0], table) == "hawleri"
    assert dialect_of(SORANI_CLIP[0], table) == "sorani"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"REVIEWER QUEUE GATE CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
