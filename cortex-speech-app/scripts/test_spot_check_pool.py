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
from review_pilot_hidden_contract import HIDDEN_SCHEMA_SQL, policy_sha256  # noqa: E402

TEST_FOCUS_IDS = ("focus-a", "focus-b")
TEST_FOCUS_CONTRACT = contract_for_ids(TEST_FOCUS_IDS)
spot_gate.verify_controlled_pilot_focus = lambda root: verify_controlled_pilot_focus(root, TEST_FOCUS_CONTRACT)


def write_test_focus(root: Path) -> None:
    (root / "voice_focus.json").write_text(
        json.dumps({"name": "test", "segment_ids": list(TEST_FOCUS_IDS)}),
        encoding="utf-8",
    )


def create_v60_pilot_history(conn: sqlite3.Connection, baseline: int) -> None:
    conn.executescript(
        f"""
        CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT);
        INSERT INTO schema_migrations VALUES(60, 'effective pilot fixture');
        CREATE TABLE review_events (
            id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, action TEXT,
            compensation_action TEXT, source TEXT, timestamp_ms INTEGER,
            created_at TEXT, duration_ms INTEGER, operation_id TEXT,
            operation_payload_hash TEXT, app_git_sha TEXT, playback_guard_version TEXT
        );
        CREATE TABLE review_effect_state (
            singleton_key INTEGER PRIMARY KEY, effective_after_review_event_id INTEGER,
            effective_after_ledger_id INTEGER, created_at TEXT
        );
        INSERT INTO review_effect_state VALUES (1, {baseline}, 0, '2026-08-22 07:00:00');
        CREATE TABLE review_compensation_ledger (
            id INTEGER PRIMARY KEY, entry_id TEXT, entry_key TEXT, policy_version TEXT,
            review_event_id INTEGER, canonical_work_id TEXT, canonical_identity_kind TEXT,
            reviewer TEXT, segment_id TEXT, source TEXT, compensation_action TEXT,
            effective_decision TEXT, decision_revision INTEGER, duration_ms INTEGER,
            rate_basis_points INTEGER, entitlement_micro_iqd INTEGER, delta_micro_iqd INTEGER,
            corrected_entitlement_ms INTEGER, delta_corrected_ms INTEGER,
            created_at TEXT, reverses_entry_id TEXT
        );
        CREATE TABLE human_decision_effect_events (
            id INTEGER PRIMARY KEY, review_event_id INTEGER, segment_id TEXT, reviewer TEXT,
            source TEXT, action TEXT, decision_revision INTEGER, created_at TEXT
        );
        CREATE TABLE human_decision_effect_reversals (
            effect_event_id INTEGER PRIMARY KEY, operation_id TEXT, created_at TEXT
        );
        CREATE VIEW effective_review_events_v60 AS
        WITH active_originals AS (
            SELECT e.id AS review_event_id, e.segment_id, e.reviewer, e.action, e.source,
                   e.timestamp_ms, e.created_at AS review_event_created_at,
                   e.duration_ms AS review_event_duration_ms,
                   e.compensation_action AS review_event_compensation_action,
                   e.operation_id, e.operation_payload_hash, e.app_git_sha,
                   e.playback_guard_version, l.id AS ledger_id, l.entry_id AS ledger_entry_id,
                   l.entry_key AS ledger_entry_key, l.policy_version, l.canonical_work_id,
                   l.canonical_identity_kind, l.reviewer AS ledger_reviewer,
                   l.segment_id AS ledger_segment_id, l.source AS ledger_source,
                   l.compensation_action AS ledger_compensation_action,
                   l.effective_decision, l.decision_revision,
                   l.duration_ms AS ledger_duration_ms, l.rate_basis_points,
                   l.entitlement_micro_iqd, l.delta_micro_iqd,
                   l.corrected_entitlement_ms, l.delta_corrected_ms,
                   l.created_at AS ledger_created_at
              FROM review_events e JOIN review_compensation_ledger l ON l.review_event_id=e.id
             WHERE l.reverses_entry_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM review_compensation_ledger r
                                WHERE r.reverses_entry_id=l.entry_id)
        )
        SELECT a.* FROM active_originals a
         WHERE NOT EXISTS (
            SELECT 1 FROM active_originals newer
             WHERE newer.policy_version=a.policy_version
               AND newer.canonical_work_id=a.canonical_work_id
               AND newer.review_event_id>a.review_event_id
         );
        """
    )
    if baseline > 0:
        conn.execute(
            """INSERT INTO review_events
                 (id, segment_id, reviewer, action, compensation_action, source,
                  timestamp_ms, created_at, duration_ms)
                 VALUES (?, 'historical-baseline', 'Historical', 'accept', NULL, 'desktop',
                         1, '2026-08-22 06:00:00', 1)""",
            (baseline,),
        )


def insert_v60_event(
    conn: sqlite3.Connection,
    event_id: int,
    segment_id: str,
    reviewer: str,
    action: str,
    source: str,
) -> None:
    operation_id = f"{event_id:08x}-0000-4000-8000-{event_id:012x}"
    conn.execute(
        """INSERT INTO review_events VALUES
             (?, ?, ?, ?, ?, ?, ?, '2026-08-22 07:00:00', 1000, ?, ?, ?, ?)""",
        (
            event_id,
            segment_id,
            reviewer,
            action,
            action,
            source,
            1_700_000_000_000 + event_id,
            operation_id,
            f"{event_id:064x}",
            "a" * 40,
            "content-hash-raw-counter-v3",
        ),
    )
    conn.execute(
        """INSERT INTO review_compensation_ledger VALUES
             (?, ?, ?, 'review-iqd-v1-2026-08-21', ?, ?, 'audio_content_hash+source_span',
              ?, ?, ?, ?, ?, ?, 1000, 0, 0, 0, 0, 0, '2026-08-22 07:00:00', NULL)""",
        (
            event_id,
            f"entry-{event_id}",
            f"review-event:{event_id}",
            event_id,
            f"pilot-work:{reviewer.lower()}:{segment_id}",
            reviewer,
            segment_id,
            source,
            action,
            action,
            event_id,
        ),
    )
    if source == "couch" and action != "skip":
        conn.execute(
            """INSERT INTO human_decision_effect_events VALUES
                 (?, ?, ?, ?, ?, ?, ?, '2026-08-22 07:00:00')""",
            (event_id, event_id, segment_id, reviewer, source, action, event_id),
        )


def delete_v60_event(conn: sqlite3.Connection, event_id: int) -> None:
    entry_ids = [
        str(row[0])
        for row in conn.execute(
            "SELECT entry_id FROM review_compensation_ledger WHERE review_event_id=?", (event_id,)
        )
    ]
    effect_ids = [
        int(row[0])
        for row in conn.execute(
            "SELECT id FROM human_decision_effect_events WHERE review_event_id=?", (event_id,)
        )
    ]
    for effect_id in effect_ids:
        conn.execute(
            "DELETE FROM human_decision_effect_reversals WHERE effect_event_id=?", (effect_id,)
        )
    conn.execute("DELETE FROM human_decision_effect_events WHERE review_event_id=?", (event_id,))
    for entry_id in entry_ids:
        conn.execute("DELETE FROM review_compensation_ledger WHERE reverses_entry_id=?", (entry_id,))
    conn.execute("DELETE FROM review_compensation_ledger WHERE review_event_id=?", (event_id,))
    conn.execute("DELETE FROM review_events WHERE id=?", (event_id,))


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
        roster = {"Roza": ["sorani"], "Rubar": ["sorani", "hawleri"]}
        table = [("Kurdish Corpora\\sorani\\", "sorani"), ("KBHP", "hawleri")]

        focused = available_keys_by_reviewer(
            reviewers=["Roza", "Rubar"],
            roster=roster,
            focus={"haw"},
            candidates=candidates,
            already_scored=set(),
            dialect_table=table,
        )
        assert focused == {"Roza": 0, "Rubar": 1}

        scored = available_keys_by_reviewer(
            reviewers=["Rubar"],
            roster=roster,
            focus={"haw"},
            candidates=candidates,
            already_scored={("haw", "rubar")},
            dialect_table=table,
        )
        assert scored == {"Rubar": 0}, "a previously answered key must not count as fresh capacity"


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
    roster = {"Roza": ["sorani"], "Rubar": ["sorani", "hawleri"]}
    table = [("Kurdish Corpora\\sorani\\", "sorani"), ("KBHP", "hawleri")]
    assert work_counts_by_reviewer(
        reviewers=["Roza", "Rubar"],
        roster=roster,
        clips=clips,
        dialect_table=table,
    ) == {"Roza": 1, "Rubar": 2}


def test_controlled_pilot_policy_is_exactly_two_reviewers_ten_each_twenty_total() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "review_pilot_policy.json"
        valid = {
            "schema_version": 1,
            "after_review_event_id": 863,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Rubar", "max_corpus_actions": 10},
                {"name": "Alle", "max_corpus_actions": 10},
            ],
        }
        path.write_text(json.dumps(valid), encoding="utf-8")
        write_test_focus(Path(tmp))
        policy = load_review_pilot_policy(Path(tmp))
        assert policy is not None
        assert policy.reviewer_caps == {"Rubar": 10, "Alle": 10}

        for mutate in (
            lambda value: value.update(max_total_corpus_actions=21),
            lambda value: value["reviewers"][0].update(max_corpus_actions=11),
            lambda value: value["reviewers"][0].update(name="Roza"),
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
            {"name": "Rubar", "max_corpus_actions": 10},
            {"name": "Alle", "max_corpus_actions": 10},
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
    policy = ReviewPilotPolicy(100, 20, {"Rubar": 10, "Alle": 10})
    conn = sqlite3.connect(":memory:")
    create_v60_pilot_history(conn, 100)
    insert_v60_event(conn, 101, "work-h-1", "Rubar", "accept", "couch")
    insert_v60_event(conn, 102, "work-h-2", "Rubar", "skip", "couch")
    insert_v60_event(conn, 103, "hidden-p-1", "Alle", "edit", "couch_spot_check")
    insert_v60_event(conn, 104, "work-p-1", "Alle", "reject", "couch")
    total, progress = pilot_progress(conn, policy)
    assert total == 3 and progress == {"Rubar": 2, "Alle": 1}
    bounded = pilot_bounded_work_counts({"Rubar": 8_215, "Alle": 8_215}, policy, progress)
    assert bounded == {"Rubar": 8, "Alle": 9}
    # The bounded ten-action sample has an exact ceil(10/8)=2 distinct-key ceiling. The independent
    # three-key floor remains mandatory only for the unrestricted long-running campaign.
    assert pilot_required_fresh_keys(10, bounded["Rubar"], 0, 25, 8) == 1
    assert pilot_required_fresh_keys(10, 10, 0, 25, 8) == 2
    assert pilot_required_fresh_keys(10, 10, 2, 25, 8) == 0
    conn.close()


def test_pilot_progress_never_refunds_a_skip_shadowed_by_a_later_decision() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Rubar": 10, "Alle": 10})
    conn = sqlite3.connect(":memory:")
    create_v60_pilot_history(conn, 0)
    insert_v60_event(conn, 1, "same-work", "Rubar", "skip", "couch")
    insert_v60_event(conn, 2, "same-work", "Rubar", "accept", "couch")
    total, progress = pilot_progress(conn, policy)
    assert total == 2 and progress["Rubar"] == 2
    conn.close()


def test_pilot_hidden_budget_is_db_authoritative_and_session_is_only_a_subset() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        db_path = root / "cortex-speech.db"
        raw_policy = {
            "schema_version": 1,
            "after_review_event_id": 100,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Rubar", "max_corpus_actions": 10},
                {"name": "Alle", "max_corpus_actions": 10},
            ],
        }
        (root / "review_pilot_policy.json").write_text(json.dumps(raw_policy), encoding="utf-8")
        write_test_focus(root)
        policy = load_review_pilot_policy(root)
        assert policy is not None
        digest = policy_sha256(policy)
        conn = sqlite3.connect(db_path)
        create_v60_pilot_history(conn, 100)
        conn.execute("CREATE TABLE spot_checks(segment_id TEXT, reviewer TEXT, action TEXT)")
        conn.executescript(HIDDEN_SCHEMA_SQL)
        conn.executemany(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 100, ?, ?)",
            [(digest, "Rubar", "h1"), (digest, "Alle", "p1")],
        )
        conn.commit()
        (root / "couch_session.json").write_text(
            json.dumps(
                {
                    "db_path": str(db_path),
                    "reviewers": {"token-h": "Rubar", "token-p": "Alle"},
                    "pilot_policy": raw_policy,
                    # A restart may lag the DB; the cache is allowed to be a strict subset.
                    "pilot_spot_checks": [["h1", "Rubar"]],
                }
            ),
            encoding="utf-8",
        )
        grants, state = load_pilot_served_checks(root, policy, conn, db_path)
        assert grants == {"Rubar": {"h1"}, "Alle": {"p1"}}
        assert state.session_keys == {"Rubar": {"h1"}, "Alle": set()}

        insert_v60_event(conn, 101, "h1", "Rubar", "accept", "couch_spot_check")
        insert_v60_event(conn, 102, "p1", "Alle", "skip", "couch_spot_check")
        conn.executemany(
            "INSERT INTO spot_checks VALUES (?, ?, ?)",
            [("h1", "Rubar", "accept"), ("p1", "Alle", "skip")],
        )
        conn.commit()
        _grants, resolved = load_pilot_served_checks(root, policy, conn, db_path)
        assert resolved.completed_keys == {"Rubar": {"h1"}, "Alle": set()}
        assert resolved.skipped_keys == {"Rubar": set(), "Alle": {"p1"}}
        assert resolved.unresolved_keys == {"Rubar": set(), "Alle": set()}
        assert resolved.total_corpus_actions == 0
        assert resolved.total_hidden_actions == 2  # hidden skip consumes one of the two hidden slots
        assert resolved.total_ui_actions == 2

        # A hidden-key skip that lands through the corpus path consumes a corpus slot and resolves
        # the durable key, but remains unable to certify.
        delete_v60_event(conn, 102)
        conn.execute("DELETE FROM spot_checks WHERE segment_id = 'p1' AND reviewer = 'Alle'")
        insert_v60_event(conn, 102, "p1", "Alle", "skip", "couch")
        conn.commit()
        _grants, legacy = load_pilot_served_checks(root, policy, conn, db_path)
        assert legacy.skipped_keys == {"Rubar": set(), "Alle": {"p1"}}
        assert legacy.total_corpus_actions == 1
        assert legacy.total_hidden_actions == 1
        assert legacy.total_ui_actions == 2

        for event_id in range(101, 103):
            delete_v60_event(conn, event_id)
        conn.execute("DELETE FROM spot_checks")
        conn.executemany(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 100, ?, ?)",
            [(digest, "Rubar", "h2"), (digest, "Alle", "p2")],
        )
        corpus_events = [
            (101 + index, f"work-h-{index}", "Rubar", "accept", "couch")
            for index in range(10)
        ] + [
            (111 + index, f"work-p-{index}", "Alle", "edit", "couch")
            for index in range(10)
        ]
        hidden_events = [
            (121, "h1", "Rubar", "accept", "couch_spot_check"),
            (122, "h2", "Rubar", "edit", "couch_spot_check"),
            (123, "p1", "Alle", "reject", "couch_spot_check"),
            (124, "p2", "Alle", "accept", "couch_spot_check"),
        ]
        for event in corpus_events + hidden_events:
            insert_v60_event(conn, *event)
        conn.executemany(
            "INSERT INTO spot_checks VALUES (?, ?, ?)",
            [
                ("h1", "Rubar", "accept"),
                ("h2", "Rubar", "edit"),
                ("p1", "Alle", "reject"),
                ("p2", "Alle", "accept"),
            ],
        )
        conn.commit()
        _grants, complete = load_pilot_served_checks(root, policy, conn, db_path)
        assert complete.total_corpus_actions == 20
        assert complete.total_hidden_actions == 4
        assert complete.total_ui_actions == 24
        insert_v60_event(conn, 125, "work-too-many", "Rubar", "skip", "couch")
        conn.commit()
        try:
            load_pilot_served_checks(root, policy, conn, db_path)
        except Exception as error:
            assert "10-action cap" in str(error), error
        else:
            raise AssertionError("a 25th controlled-pilot UI action was accepted")
        delete_v60_event(conn, 125)
        conn.commit()

        broken = json.loads((root / "couch_session.json").read_text(encoding="utf-8"))
        broken["pilot_spot_checks"] = [["never-reserved", "Alle"]]
        (root / "couch_session.json").write_text(json.dumps(broken), encoding="utf-8")
        try:
            load_pilot_served_checks(root, policy, conn, db_path)
        except Exception:
            pass
        else:
            raise AssertionError("session cache was allowed to mint an unreserved hidden key")
        conn.close()


def test_policy_digest_is_semantic_and_matches_the_rust_byte_contract() -> None:
    canonical = ReviewPilotPolicy(863, 20, {"Rubar": 10, "Alle": 10})
    reordered_and_recased = ReviewPilotPolicy(863, 20, {"aLlE": 10, "RUBAR": 10})
    assert policy_sha256(canonical) == "2d5d5ce3c0344e8be93540cfd4d0ed5f229e9ece16495bca219075d305303bd2"
    assert policy_sha256(reordered_and_recased) == policy_sha256(canonical)


def test_pilot_history_with_an_unauthorized_reviewer_fails_closed() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Rubar": 10, "Alle": 10})
    conn = sqlite3.connect(":memory:")
    create_v60_pilot_history(conn, 0)
    insert_v60_event(conn, 1, "work-s", "Sewa", "accept", "couch")
    try:
        pilot_progress(conn, policy)
    except Exception:
        pass
    else:
        raise AssertionError("an unauthorized post-baseline reviewer was ignored")
    conn.close()


def test_ten_direct_actions_with_zero_checks_are_red_until_exact_two_results_exist() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Rubar": 10, "Alle": 10})
    conn = sqlite3.connect(":memory:")
    create_v60_pilot_history(conn, 0)
    conn.execute(
        "CREATE TABLE spot_checks (segment_id TEXT, reviewer TEXT, noticed INTEGER, cer REAL)"
    )
    for index in range(1, 11):
        insert_v60_event(conn, index, f"work-{index}", "Rubar", "accept", "couch")
    total, progress = pilot_progress(conn, policy)
    assert total == 10 and progress["Rubar"] == 10
    served = {"Rubar": set(), "Alle": set()}
    issues = pilot_certification_issues(conn, policy, progress, served, 8)
    assert any("0/2 pilot keys" in issue for issue in issues)
    assert any("0/2 pilot results" in issue for issue in issues)
    assert pilot_required_fresh_keys(10, 0, 0, 25, 8, at_action_cap=True) == 2

    served["Rubar"] = {"key-1", "key-2"}
    conn.executemany(
        "INSERT INTO spot_checks VALUES (?, 'Rubar', 1, 0.0)",
        [("key-1",), ("key-2",)],
    )
    assert pilot_certification_issues(conn, policy, progress, served, 8) == []
    assert pilot_required_fresh_keys(10, 0, 2, 25, 8, at_action_cap=True) == 0

    conn.execute("UPDATE spot_checks SET noticed = 0, cer = 1.0 WHERE segment_id = 'key-2'")
    assert any("failed 1/2" in issue for issue in pilot_certification_issues(conn, policy, progress, served, 8))
    conn.close()


def test_hidden_skip_consumes_qc_slot_but_can_never_certify() -> None:
    policy = ReviewPilotPolicy(0, 20, {"Rubar": 10, "Alle": 10})
    conn = sqlite3.connect(":memory:")
    create_v60_pilot_history(conn, 0)
    conn.execute(
        "CREATE TABLE spot_checks (segment_id TEXT, reviewer TEXT, noticed INTEGER, cer REAL)"
    )
    insert_v60_event(conn, 1, "hidden-skip", "Rubar", "skip", "couch_spot_check")
    conn.execute("INSERT INTO spot_checks VALUES ('hidden-skip', 'Rubar', 0, 1.0)")
    total, progress = pilot_progress(conn, policy)
    assert total == 0 and progress["Rubar"] == 0, "hidden QC must not consume corpus quota"
    issues = pilot_certification_issues(
        conn,
        policy,
        progress,
        {"Rubar": {"hidden-skip"}, "Alle": set()},
        8,
    )
    assert any("skipped 1 hidden check" in issue for issue in issues), issues
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
