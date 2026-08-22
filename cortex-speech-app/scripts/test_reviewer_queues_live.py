#!/usr/bin/env python3
"""Unit tests for the reviewer-queue gate's pure decision core.

The gate itself reads the live machine; these pin the logic that decides, given a roster and a set of
clips, whether anybody is about to open a dead link. Both incidents of 2026-08-17 appear here as
fixtures, so a regression of either fails here rather than on a reviewer's phone.
"""

from __future__ import annotations

import json
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_reviewer_queues_live import (  # noqa: E402
    PolicyBroken,
    allowed_for,
    dialect_of,
    evaluate_queues,
    load_focus,
    load_roster,
    may_judge,
    source_dialects,
    wrong_dialect_decisions,
)

# Plain strings, not r"" — a Python raw string may not END in a backslash, and these fragments all do
# (the trailing separator is what keeps `sorani-hawleri\` from matching the `sorani\` entry).
TABLE = [
    ("KBHP", "hawleri"),
    ("Kurdish Corpora\\sorani-hawleri\\", "hawleri"),
    ("Kurdish Corpora\\sorani\\", "sorani"),
    ("Kurdish Corpora\\badini\\", "badini"),
]

# Drive-letter path with no profile name in it: a tracked file may not carry a private local path
# (test_windows_repo_hygiene), and the matcher looks at the KBHP fragment, not the prefix.
HAWLERI_CLIP = (r"D:\corpora\_batch_remaining\KBHP-EP01_0007.wav", 9000)
SORANI_CLIP = (r"D:\Kurdish Corpora\sorani\ZarPodcast\2\A1-0050.wav", 7000)


def test_wrong_dialect_gate_reads_current_attribution_not_superseded_history() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        db_path = Path(tmp) / "queue.db"
        con = sqlite3.connect(db_path)
        try:
            con.executescript(
                """
                CREATE TABLE speech_segments (
                    id TEXT PRIMARY KEY,
                    audio_path TEXT NOT NULL,
                    verified INTEGER NOT NULL,
                    human_decision TEXT,
                    reviewed_by TEXT
                );
                CREATE TABLE review_events (
                    reviewer TEXT NOT NULL,
                    segment_id TEXT NOT NULL,
                    action TEXT NOT NULL
                );
                """
            )
            # Historical Alle edit, now superseded by an unattributed legacy reject: it is neither
            # current attribution nor downstream training data and must not accuse Alle today.
            con.execute(
                "INSERT INTO speech_segments VALUES (?, ?, 1, 'reject', NULL)",
                ("superseded", HAWLERI_CLIP[0]),
            )
            con.execute("INSERT INTO review_events VALUES ('Alle', 'superseded', 'edit')")

            # Current attribution is authoritative. Both a usable accept and an excluded reject are
            # routing violations when a Sorani-only reviewer judges Hawleri.
            con.execute(
                "INSERT INTO speech_segments VALUES (?, ?, 1, 'accept', 'Alle')",
                ("current-accept", HAWLERI_CLIP[0]),
            )
            con.execute(
                "INSERT INTO speech_segments VALUES (?, ?, 1, 'reject', 'Alle')",
                ("current-reject", HAWLERI_CLIP[0]),
            )
            con.execute(
                "INSERT INTO speech_segments VALUES (?, ?, 1, 'edit', 'Hawzhin')",
                ("allowed", HAWLERI_CLIP[0]),
            )
            con.commit()
        finally:
            con.close()

        assert wrong_dialect_decisions(
            db_path,
            {"Alle": ["sorani"], "Hawzhin": ["hawleri"]},
            TABLE,
        ) == {"Alle": 2}


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
    assert dialect_of(r"d:\KURDISH CORPORA\SORANI\zarpodcast\a.wav", TABLE) == "sorani"
    assert dialect_of(r"d:\kurdish corpora\SORANI-HAWLERI\kbhp\ep1.wav", TABLE) == "hawleri"


def test_a_comment_key_does_not_empty_the_roster() -> None:
    # The second half of the incident: a helpful "_comment" string made a strict parse fail, and the
    # failure path is "unrestricted" — so the whole protection switched off silently.
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text(
            json.dumps({"_comment": "how to edit", "Roza": ["sorani"]}), encoding="utf-8"
        )
        roster = load_roster(Path(tmp))
    assert roster == {"Roza": ["sorani"]}, roster


def test_a_broken_policy_file_fails_the_gate_instead_of_unrestricting_everyone() -> None:
    # Owner instruction 2026-08-20: present-but-broken fails CLOSED. The server 503s every queue, so
    # the gate must FAIL loudly — the old mirror returned "unrestricted"/"no focus" and would have
    # counted thousands of servable clips against links that serve nothing.
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text("{ not json", encoding="utf-8")
        try:
            load_roster(Path(tmp))
            raise AssertionError("a roster that is not JSON must raise PolicyBroken")
        except PolicyBroken:
            pass
    with tempfile.TemporaryDirectory() as tmp:
        # A typo'd RESTRICTION: skipping it silently un-restricts exactly the reviewer it names.
        (Path(tmp) / "reviewer_dialects.json").write_text(json.dumps({"Roza": "sorani"}), encoding="utf-8")
        try:
            load_roster(Path(tmp))
            raise AssertionError("a typo'd roster entry must raise PolicyBroken")
        except PolicyBroken as e:
            assert "Roza" in str(e), e
    for broken in ("{ not json", json.dumps({"name": "V"}), json.dumps({"name": "V", "segment_ids": []})):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "voice_focus.json").write_text(broken, encoding="utf-8")
            try:
                load_focus(Path(tmp))
                raise AssertionError(f"a broken focus must raise PolicyBroken: {broken!r}")
            except PolicyBroken:
                pass


def test_a_roster_key_binds_across_case_and_whitespace_and_duplicates_are_broken() -> None:
    # 2026-08-20 hunt: the exact-match lookup let an orphaned key ("roza " vs live "Roza") load
    # cleanly and bind NOBODY — its reviewer served unrestricted, the wrong-dialect incident back
    # through a typo'd key. The lookup now matches the way the session layer matches names.
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text(json.dumps({"roza ": ["sorani"]}), encoding="utf-8")
        roster = load_roster(Path(tmp))
        assert allowed_for(roster, "Roza") == ["sorani"], "the key must bind its reviewer across case/space"
        assert allowed_for(roster, "Rubar") is None
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text(
            json.dumps({"Roza": ["sorani"], "roza": ["hawleri"]}), encoding="utf-8"
        )
        try:
            load_roster(Path(tmp))
            raise AssertionError("case-colliding keys are one broken file")
        except PolicyBroken:
            pass


def test_roster_values_are_normalized_and_unknown_or_empty_dialects_are_broken() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text(
            json.dumps({"Roza": [" Sorani ", "SORANI", "Hawleri"]}), encoding="utf-8"
        )
        assert load_roster(Path(tmp)) == {"Roza": ["sorani", "hawleri"]}

    for invalid in ({"Roza": ["soranii"]}, {"Roza": []}):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "reviewer_dialects.json").write_text(json.dumps(invalid), encoding="utf-8")
            try:
                load_roster(Path(tmp))
                raise AssertionError(f"invalid dialect roster must raise PolicyBroken: {invalid!r}")
            except PolicyBroken:
                pass


def test_nonfinite_json_is_broken_exactly_as_the_server_sees_it() -> None:
    # Python's json accepts (and emits!) NaN/Infinity; serde_json 503s them. The mirror must refuse
    # the same bytes the server refuses, or the gate reads OK against a dead queue.
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "voice_focus.json").write_text('{"name":"V","score":Infinity,"segment_ids":["a"]}', encoding="utf-8")
        try:
            load_focus(Path(tmp))
            raise AssertionError("Infinity parses in Python but 503s the server — must be PolicyBroken")
        except PolicyBroken:
            pass
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "reviewer_dialects.json").write_text('{"_note": NaN, "Sara": ["sorani"]}', encoding="utf-8")
        try:
            load_roster(Path(tmp))
            raise AssertionError("NaN parses in Python but 503s the server — must be PolicyBroken")
        except PolicyBroken:
            pass


def test_live_reviewers_mirrors_the_servers_own_session_authority() -> None:
    from check_reviewer_queues_live import live_reviewers

    db = Path("C:/anywhere/cortex-speech.db")
    with tempfile.TemporaryDirectory() as tmp:
        d = Path(tmp)
        (d / "couch_session.json").write_text(
            json.dumps({"db_path": str(db), "reviewers": {"tok": "Sara"}}), encoding="utf-8"
        )
        assert live_reviewers(d, db) == ["Sara"], "a plain remembered session is live"
        # Stop writes the revocation marker FIRST and may fail to delete the file: marker wins.
        (d / "couch_session.revoked").write_text("revoked\n", encoding="utf-8")
        assert live_reviewers(d, db) == [], "the revocation marker is authoritative — no links are live"
        (d / "couch_session.revoked").unlink()
        # A session remembered against a DIFFERENT library never resumes.
        assert live_reviewers(d, Path("C:/other/lib.db")) == [], "wrong library = not live"
        # An unreadable file is a question, not an all-clear.
        (d / "couch_session.json").write_text("{ not json", encoding="utf-8")
        try:
            live_reviewers(d, db)
            raise AssertionError("an unreadable session file must FAIL, never read as 'no links'")
        except PolicyBroken:
            pass


def test_missing_policy_files_still_mean_unrestricted() -> None:
    # The other half of the contract is unchanged: ABSENCE is the state before the files existed.
    with tempfile.TemporaryDirectory() as tmp:
        assert load_roster(Path(tmp)) == {}
        assert load_focus(Path(tmp)) is None


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
