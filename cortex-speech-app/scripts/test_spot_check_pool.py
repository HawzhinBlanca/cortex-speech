#!/usr/bin/env python3
"""Regression tests for the live hidden-check capacity gate."""

from __future__ import annotations

import sys
import tempfile
import sqlite3
import json
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_spot_check_pool as spot_gate  # noqa: E402
from check_spot_check_pool import (  # noqa: E402
    available_keys_by_reviewer,
    learning_key,
    load_pilot_served_checks,
    load_review_pilot_policy,
    pilot_bounded_work_counts,
    pilot_certification_issues,
    pilot_progress,
    pilot_required_fresh_keys,
    ReviewPilotPolicy,
    required_keys_for_work,
    serving_constants,
    work_counts_by_reviewer,
)
from pilot_focus_contract import contract_for_ids, verify_controlled_pilot_focus  # noqa: E402

TEST_FOCUS_IDS = ("focus-a", "focus-b")
TEST_FOCUS_CONTRACT = contract_for_ids(TEST_FOCUS_IDS)
spot_gate.verify_controlled_pilot_focus = lambda root: verify_controlled_pilot_focus(root, TEST_FOCUS_CONTRACT)


def write_test_focus(root: Path) -> None:
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "test", "segment_ids": list(TEST_FOCUS_IDS)}),
        encoding="utf-8",
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


def test_controlled_pilot_policy_is_exactly_two_reviewers_ten_each_twenty_total() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "review_pilot_policy.json"
        valid = {
            "schema_version": 1,
            "after_review_event_id": 863,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Hawzhin", "max_corpus_actions": 10},
                {"name": "Pavel", "max_corpus_actions": 10},
            ],
        }
        path.write_text(json.dumps(valid), encoding="utf-8")
        write_test_focus(Path(tmp))
        policy = load_review_pilot_policy(Path(tmp))
        assert policy is not None
        assert policy.reviewer_caps == {"Hawzhin": 10, "Pavel": 10}

        for mutate in (
            lambda value: value.update(max_total_corpus_actions=21),
            lambda value: value["reviewers"][0].update(max_corpus_actions=11),
            lambda value: value.update(typo=True),
            lambda value: value.update(schema_version=True),
            lambda value: value.update(after_review_event_id=True),
            lambda value: value.update(max_total_corpus_actions=True),
            lambda value: value["reviewers"][0].update(max_corpus_actions=True),
            lambda value: value["reviewers"][0].update(typo=True),
        ):
            broken = json.loads(json.dumps(valid))
            mutate(broken)
            path.write_text(json.dumps(broken), encoding="utf-8")
            try:
                load_review_pilot_policy(Path(tmp))
            except Exception:
                pass
            else:
                raise AssertionError(f"weakened pilot policy was accepted: {broken}")

        for invalid_json in (
            '{"schema_version":1,"schema_version":1}',
            '{"schema_version":NaN}',
        ):
            path.write_text(invalid_json, encoding="utf-8")
            try:
                load_review_pilot_policy(Path(tmp))
            except Exception:
                pass
            else:
                raise AssertionError(f"non-serde JSON was accepted: {invalid_json}")


def test_controlled_pilot_spot_gate_refuses_missing_or_wrong_focus() -> None:
    valid = {
        "schema_version": 1,
        "after_review_event_id": 863,
        "max_total_corpus_actions": 20,
        "reviewers": [
            {"name": "Hawzhin", "max_corpus_actions": 10},
            {"name": "Pavel", "max_corpus_actions": 10},
        ],
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "review_pilot_policy.json").write_text(json.dumps(valid), encoding="utf-8")
        for ids, expected in ((None, "is required"), (["focus-a", "focus-wrong"], "digest mismatch")):
            if ids is not None:
                (root / "voice_focus.json").write_text(json.dumps({"segment_ids": ids}), encoding="utf-8")
            try:
                load_review_pilot_policy(root)
            except Exception as error:
                assert expected in str(error), error
            else:
                raise AssertionError("spot-check gate accepted a missing or wrong controlled-pilot focus")
        write_test_focus(root)
        assert load_review_pilot_policy(root) is not None


def test_pilot_progress_and_hidden_capacity_derive_from_the_enforced_cap() -> None:
    policy = ReviewPilotPolicy(100, 20, {"Hawzhin": 10, "Pavel": 10})
    conn = sqlite3.connect(":memory:")
    conn.execute(
        "CREATE TABLE review_events (id INTEGER PRIMARY KEY, reviewer TEXT, action TEXT, source TEXT)"
    )
    conn.executemany(
        "INSERT INTO review_events VALUES (?, ?, ?, ?)",
        [
            (99, "Someone", "accept", "couch"),  # before the immutable baseline
            (101, "Hawzhin", "accept", "couch"),
            (102, "Hawzhin", "skip", "couch"),  # zero pay/canary, but consumes one safety slot
            (103, "Pavel", "edit", "couch_spot_check"),  # hidden check, not corpus work
            (104, "Pavel", "reject", "couch"),
        ],
    )
    total, progress = pilot_progress(conn, policy)
    assert total == 3 and progress == {"Hawzhin": 2, "Pavel": 1}
    bounded = pilot_bounded_work_counts({"Hawzhin": 8_215, "Pavel": 8_215}, policy, progress)
    assert bounded == {"Hawzhin": 8, "Pavel": 9}
    # The bounded ten-action sample has an exact ceil(10/8)=2 distinct-key ceiling. The independent
    # three-key floor remains mandatory only for the unrestricted long-running campaign.
    assert pilot_required_fresh_keys(10, bounded["Hawzhin"], 0, 25, 8) == 1
    assert pilot_required_fresh_keys(10, 10, 0, 25, 8) == 2
    assert pilot_required_fresh_keys(10, 10, 2, 25, 8) == 0
    conn.close()


def test_pilot_served_key_budget_is_session_bound_and_cannot_reset_on_reload() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        raw_policy = {
            "schema_version": 1,
            "after_review_event_id": 100,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Hawzhin", "max_corpus_actions": 10},
                {"name": "Pavel", "max_corpus_actions": 10},
            ],
        }
        (root / "review_pilot_policy.json").write_text(json.dumps(raw_policy), encoding="utf-8")
        write_test_focus(root)
        (root / "couch_session.json").write_text(
            json.dumps(
                {
                    "pilot_policy": raw_policy,
                    "pilot_spot_checks": [
                        ["h1", "Hawzhin"],
                        ["h1", "hawzhin"],
                        ["p1", "Pavel"],
                    ],
                }
            ),
            encoding="utf-8",
        )
        policy = load_review_pilot_policy(root)
        assert policy is not None
        assert load_pilot_served_checks(root, policy) == {"Hawzhin": {"h1"}, "Pavel": {"p1"}}

        broken = json.loads((root / "couch_session.json").read_text(encoding="utf-8"))
        broken["pilot_policy"]["after_review_event_id"] = 99
        (root / "couch_session.json").write_text(json.dumps(broken), encoding="utf-8")
        try:
            load_pilot_served_checks(root, policy)
        except Exception:
            pass
        else:
            raise AssertionError("a session bound to a different pilot baseline was accepted")


def test_pilot_history_with_an_unauthorized_reviewer_fails_closed() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Hawzhin": 10, "Pavel": 10})
    conn = sqlite3.connect(":memory:")
    conn.execute(
        "CREATE TABLE review_events (id INTEGER PRIMARY KEY, reviewer TEXT, action TEXT, source TEXT)"
    )
    conn.execute("INSERT INTO review_events VALUES (1, 'Rubar', 'accept', 'couch')")
    try:
        pilot_progress(conn, policy)
    except Exception:
        pass
    else:
        raise AssertionError("an unauthorized post-baseline reviewer was ignored")
    conn.close()


def test_ten_direct_actions_with_zero_checks_are_red_until_exact_two_results_exist() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Hawzhin": 10, "Pavel": 10})
    conn = sqlite3.connect(":memory:")
    conn.execute(
        "CREATE TABLE review_events (id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, action TEXT, source TEXT)"
    )
    conn.execute(
        "CREATE TABLE spot_checks (segment_id TEXT, reviewer TEXT, noticed INTEGER, cer REAL)"
    )
    conn.executemany(
        "INSERT INTO review_events VALUES (?, ?, 'Hawzhin', 'accept', 'couch')",
        [(index, f"work-{index}") for index in range(1, 11)],
    )
    total, progress = pilot_progress(conn, policy)
    assert total == 10 and progress["Hawzhin"] == 10
    served = {"Hawzhin": set(), "Pavel": set()}
    issues = pilot_certification_issues(conn, policy, progress, served, 8)
    assert any("0/2 pilot keys" in issue for issue in issues)
    assert any("0/2 pilot results" in issue for issue in issues)
    assert pilot_required_fresh_keys(10, 0, 0, 25, 8, at_action_cap=True) == 2

    served["Hawzhin"] = {"key-1", "key-2"}
    conn.executemany(
        "INSERT INTO spot_checks VALUES (?, 'Hawzhin', 1, 0.0)",
        [("key-1",), ("key-2",)],
    )
    assert pilot_certification_issues(conn, policy, progress, served, 8) == []
    assert pilot_required_fresh_keys(10, 0, 2, 25, 8, at_action_cap=True) == 0

    conn.execute("UPDATE spot_checks SET noticed = 0, cer = 1.0 WHERE segment_id = 'key-2'")
    assert any("failed 1/2" in issue for issue in pilot_certification_issues(conn, policy, progress, served, 8))
    conn.close()


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"SPOT-CHECK GATE CORE: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
