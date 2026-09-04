"""The readiness gate must keep agreeing with the Rust it claims to reproduce — and must stay refusable.

`check_playback_enforcement_readiness.py` re-implements `db::has_sufficient_playback_evidence` in
Python so the owner can ask "is enforcement safe to turn on?" without a build. A re-implementation
that drifts from the original answers a question nobody asked, so the bar and the policy version are
pinned to the Rust constants here.

The last pin is the one that matters most. The gate exists BECAUSE an empty log read as a pass on
2026-08-19: the binary predated the guard, no phone decision had been taken since receipts began, and
"no reviewer would have been refused" was true only in the way that every statement about an empty
set is true. A gate that cannot return NOT READY on that exact input is the trap it was written to
close, so it is driven against a fabricated-clean database and required to refuse.
"""

from __future__ import annotations

import importlib.util
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

from _command_policy_util import command_surface
from _couch_policy_util import couch_surface
from _db_policy_util import database_surface

REPO_ROOT = Path(__file__).resolve().parents[1]
GATE = REPO_ROOT / "scripts" / "check_playback_enforcement_readiness.py"
DB_RS = REPO_ROOT / "src-tauri" / "src" / "db.rs"
COUCH_RS = REPO_ROOT / "src-tauri" / "src" / "couch.rs"
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"
SEGMENTS_WRITE_RS = REPO_ROOT / "src-tauri" / "src" / "commands" / "segments_write.rs"
CERTIFICATION_GATE = REPO_ROOT / "scripts" / "check_review_pilot_certification.py"
VERIFY_10 = REPO_ROOT.parent / "scripts" / "verify_10.py"
REQUEUE_TOOL = REPO_ROOT / "scripts" / "requeue_unheard_decisions.py"
LEGACY_REPAIR_TOOL = REPO_ROOT / "scripts" / "repair_unfinalized_reviews.py"

sys.path.insert(0, str(GATE.parent))
import check_playback_enforcement_readiness as gate  # noqa: E402
import repair_unfinalized_reviews as repair  # noqa: E402
import requeue_unheard_decisions as requeue  # noqa: E402

EVENT_TIMESTAMP_MS = 1_700_000_001_000
CONTENT_HASH = "a" * 64
OTHER_CONTENT_HASH = "b" * 64


def _rust_const(name: str, source: Path) -> str:
    text = database_surface(REPO_ROOT / "src-tauri" / "src") if source == DB_RS else source.read_text(encoding="utf-8")
    match = re.search(rf"pub const {name}:\s*\w+\s*=\s*([0-9.]+);", text)
    assert match, f"{name} is gone from {source.name} — the gate now pins a constant that no longer exists"
    return match.group(1)


def test_the_coverage_bar_matches_the_rust_constant() -> None:
    assert float(_rust_const("MIN_PLAYBACK_COVERAGE", DB_RS)) == gate.MIN_PLAYBACK_COVERAGE


def test_the_policy_version_matches_the_rust_constant() -> None:
    assert int(_rust_const("PLAYBACK_POLICY_VERSION", DB_RS)) == gate.PLAYBACK_POLICY_VERSION


def test_source_span_duration_tolerance_matches_the_rust_constant() -> None:
    from pilot_focus_contract import MAX_SOURCE_SPAN_DURATION_DELTA_MS

    assert (
        int(_rust_const("MAX_SOURCE_SPAN_DURATION_DELTA_MS", DB_RS))
        == MAX_SOURCE_SPAN_DURATION_DELTA_MS
        == 1
    )


def test_runtime_and_repair_tools_never_authorize_from_the_stored_ratio() -> None:
    rust = database_surface(REPO_ROOT / "src-tauri" / "src")
    start = rust.index("fn has_sufficient_playback_evidence_on(")
    end = rust.index("\n/// Re-derive one historical policy-4 authority", start)
    authorization = rust[start:end]
    assert "coverage_ratio" not in authorization
    assert "played_ms" in authorization and "clip_duration_ms" in authorization
    assert "PLAYBACK_POLICY_VERSION" in authorization
    assert "s.audio_content_hash" in authorization
    assert "s.audio_fingerprint" not in authorization
    assert "source_start_ms" in authorization and "source_end_ms" in authorization
    assert "s.alignment_json" in authorization
    for path in (REQUEUE_TOOL, LEGACY_REPAIR_TOOL):
        source = path.read_text(encoding="utf-8")
        assert "MAX(coverage_ratio)" not in source
        assert "uncovered" in source
    requeue_source = REQUEUE_TOOL.read_text(encoding="utf-8")
    assert "UPDATE speech_segments" not in requeue_source
    assert "DELETE FROM agent_examples" not in requeue_source
    assert "offline --apply cannot atomically reverse every decision side effect" in requeue_source


def test_playback_identity_has_no_segment_id_or_path_fallback() -> None:
    sources = [
        (DB_RS.name, database_surface(REPO_ROOT / "src-tauri" / "src")),
        (COUCH_RS.name, couch_surface(REPO_ROOT / "src-tauri" / "src")),
        ("command surface", command_surface(REPO_ROOT / "src-tauri" / "src")),
        (SEGMENTS_WRITE_RS.name, SEGMENTS_WRITE_RS.read_text(encoding="utf-8")),
        (GATE.name, GATE.read_text(encoding="utf-8")),
        (CERTIFICATION_GATE.name, CERTIFICATION_GATE.read_text(encoding="utf-8")),
    ]
    for label, source in sources:
        assert "'id:' ||" not in source, f"{label} still invents SQL identity from a segment id"
        assert 'format!("id:' not in source, f"{label} still invents identity from a segment id"
        assert 'format!("path:' not in source, f"{path.name} still substitutes a path for audio identity"


def test_the_refusal_marker_is_the_string_the_server_actually_logs() -> None:
    assert gate.ENFORCE_MARKER.decode() in couch_surface(REPO_ROOT / "src-tauri" / "src"), (
        "the gate greps the binary for a marker the server no longer emits, so it would pass on silence"
    )


def _seed(db_path: Path) -> None:
    """A database whose only phone decisions are OLD, with no receipts — the 2026-08-19 shape."""
    conn = sqlite3.connect(db_path)
    conn.executescript(
        """
        CREATE TABLE speech_segments (id TEXT PRIMARY KEY, review_revision INTEGER,
                                      audio_fingerprint TEXT, audio_content_hash TEXT,
                                      duration_ms INTEGER, alignment_json TEXT);
        CREATE TABLE review_events (id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT,
                                    reviewer TEXT, action TEXT, source TEXT, created_at TEXT,
                                    timestamp_ms INTEGER);
        CREATE TABLE review_compensation_ledger (review_event_id INTEGER, policy_version TEXT,
                                                 decision_revision INTEGER, segment_id TEXT,
                                                 reviewer TEXT, source TEXT);
        CREATE TABLE playback_receipts (id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT,
                                        segment_revision INTEGER, audio_fingerprint TEXT,
                                        coverage_ratio REAL, created_at TEXT, reviewer TEXT,
                                        played_ms INTEGER, clip_duration_ms INTEGER, policy_version INTEGER,
                                        started_at_ms INTEGER, source_start_ms INTEGER,
                                        source_end_ms INTEGER);
        INSERT INTO speech_segments VALUES ('s1', 1, '424242',
                                             'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                                             1000, '{"source_start_ms":0,"source_end_ms":1000}');
        INSERT INTO review_events
            (segment_id, reviewer, action, source, created_at, timestamp_ms)
            VALUES ('s1', 'Sara', 'accept', 'couch', '2026-08-18 21:19:20', 1700000001000);
        INSERT INTO review_compensation_ledger VALUES
            (1, 'review-iqd-v1-2026-08-21', 1, 's1', 'Sara', 'couch');
        """
    )
    conn.commit()
    conn.close()


def _insert_receipt(
    conn: sqlite3.Connection,
    segment_id: str,
    revision: int,
    fingerprint: str,
    coverage: float,
    created_at: str,
    reviewer: str | None,
    *,
    played_ms: int | None = None,
    duration_ms: int = 1000,
    policy_version: int = gate.PLAYBACK_POLICY_VERSION,
    started_at_ms: int = EVENT_TIMESTAMP_MS - 1,
    source_start_ms: int = 0,
    source_end_ms: int | None = None,
    omit_source_span: bool = False,
) -> None:
    played = round(coverage * duration_ms) if played_ms is None else played_ms
    receipt_start = None if omit_source_span else source_start_ms
    receipt_end = None if omit_source_span else (
        source_start_ms + duration_ms if source_end_ms is None else source_end_ms
    )
    conn.execute(
        """INSERT INTO playback_receipts
               (segment_id, segment_revision, audio_fingerprint, coverage_ratio, created_at,
                reviewer, played_ms, clip_duration_ms, policy_version, started_at_ms,
                source_start_ms, source_end_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        (
            segment_id,
            revision,
            fingerprint,
            coverage,
            created_at,
            reviewer,
            played,
            duration_ms,
            policy_version,
            started_at_ms,
            receipt_start,
            receipt_end,
        ),
    )


def _activate_pool_with_decision(
    db_path: Path, *, guard: str = gate.PLAYBACK_GUARD_VERSION, with_pool_decision: bool = True
) -> None:
    """Activate the flexible pool over the seeded library.

    The seeded canonical first opinion (`s1` by Sara, ledger revision 1) is what the paid path writes
    for a pool clip nobody has judged yet, so it is covered here by a revision-0 receipt taken before
    the event. The optional pool row is the SECOND opinion, the only thing the pool table ever holds.
    """
    conn = sqlite3.connect(db_path)
    conn.executescript(
        """
        CREATE TABLE review_pool_registry(
            singleton_key INTEGER, pool_id TEXT, focus_segment_count INTEGER, focus_sha256 TEXT
        );
        CREATE TABLE review_pool_members(pool_id TEXT, segment_id TEXT);
        CREATE TABLE review_pool_decisions(
            id INTEGER PRIMARY KEY, pool_id TEXT, segment_id TEXT, reviewer TEXT, action TEXT,
            submitted_transcript TEXT, served_transcript TEXT, served_revision INTEGER,
            audio_content_hash TEXT, source_start_ms INTEGER, source_end_ms INTEGER,
            duration_ms INTEGER, requested_action TEXT, requested_transcript TEXT,
            operation_id TEXT, operation_payload_hash TEXT, app_git_sha TEXT,
            playback_guard_version TEXT, created_at_ms INTEGER
        );
        CREATE TABLE review_pool_reversals(id INTEGER);
        CREATE VIEW effective_review_pool_decisions_v62 AS SELECT * FROM review_pool_decisions;
        INSERT INTO review_pool_registry VALUES(
            1, '123e4567-e89b-42d3-a456-426614174000', 1,
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
        );
        INSERT INTO review_pool_members VALUES
            ('123e4567-e89b-42d3-a456-426614174000', 's1');
        """
    )
    _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.97, "2023-11-14 22:13:20", "Sara")
    if not with_pool_decision:
        conn.commit()
        conn.close()
        return
    conn.execute(
        """INSERT INTO review_pool_decisions VALUES(
               1, '123e4567-e89b-42d3-a456-426614174000', 's1', 'Sara', 'accept',
               'دەق', 'دەق', 1,
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               0, 1000, 1000, 'accept', 'دەق',
               '11111111-1111-4111-8111-111111111111',
               'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
               'cccccccccccccccccccccccccccccccccccccccc', ?, ?)""",
        (guard, EVENT_TIMESTAMP_MS),
    )
    _insert_receipt(
        conn,
        "s1",
        1,
        CONTENT_HASH,
        0.9,
        "2023-11-14 22:13:20",
        "Sara",
        started_at_ms=EVENT_TIMESTAMP_MS - 500,
    )
    conn.commit()
    conn.close()


def _insert_event(
    conn: sqlite3.Connection,
    segment_id: str,
    reviewer: str,
    action: str,
    created_at: str,
    *,
    decision_revision: int,
    timestamp_ms: int = EVENT_TIMESTAMP_MS,
) -> int:
    cursor = conn.execute(
        """INSERT INTO review_events
               (segment_id, reviewer, action, source, created_at, timestamp_ms)
             VALUES (?, ?, ?, 'couch', ?, ?)""",
        (segment_id, reviewer, action, created_at, timestamp_ms),
    )
    event_id = cursor.lastrowid
    assert event_id is not None
    conn.execute(
        """INSERT INTO review_compensation_ledger
               (review_event_id, policy_version, decision_revision, segment_id, reviewer, source)
             VALUES (?, 'review-iqd-v1-2026-08-21', ?, ?, ?, 'couch')""",
        (event_id, decision_revision, segment_id, reviewer),
    )
    return event_id


def _operational_connection(*, cutoff: int) -> sqlite3.Connection:
    conn = sqlite3.connect(":memory:")
    conn.executescript(
        f"""
        CREATE TABLE speech_segments (
            id TEXT PRIMARY KEY, review_revision INTEGER, audio_fingerprint TEXT,
            audio_content_hash TEXT, duration_ms INTEGER,
            verified INTEGER, human_decision TEXT, verdict TEXT, corrected_at TEXT, reviewed_by TEXT,
            annotated_transcript TEXT, verdict_transcript TEXT, rationale TEXT, evidence_json TEXT,
            agreement_score REAL, escalated INTEGER, updated_at TEXT,
            alignment_json TEXT DEFAULT '{{"source_start_ms":0,"source_end_ms":1000}}'
        );
        CREATE TABLE review_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT, reviewer TEXT, action TEXT,
            source TEXT, created_at TEXT, timestamp_ms INTEGER
        );
        CREATE TABLE review_compensation_ledger (
            review_event_id INTEGER, policy_version TEXT, decision_revision INTEGER,
            segment_id TEXT, reviewer TEXT, source TEXT
        );
        CREATE TABLE playback_receipts (
            id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT, segment_revision INTEGER,
            audio_fingerprint TEXT, coverage_ratio REAL, created_at TEXT, reviewer TEXT,
            played_ms INTEGER, clip_duration_ms INTEGER, policy_version INTEGER, started_at_ms INTEGER,
            source_start_ms INTEGER, source_end_ms INTEGER
        );
        CREATE TABLE decision_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT, segment_id TEXT, decision_type TEXT,
            timestamp_ms INTEGER, human_decision TEXT, created_at TEXT
        );
        CREATE TABLE review_compensation_policies (policy_version TEXT, effective_after_event_id INTEGER);
        CREATE TABLE agent_examples (segment_id TEXT);
        INSERT INTO review_compensation_policies VALUES ('review-iqd-v1-2026-08-21', {cutoff});
        """
    )
    return conn


def _run(db_path: Path, exe: Path, since: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe), "--since", since],
        capture_output=True,
        text=True,
    )


def test_an_empty_window_is_refused_not_passed() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")  # a binary that CAN warn
        result = _run(db_path, exe, "2026-08-19 05:50:31")
        assert result.returncode == 1, f"an empty window must not read as ready:\n{result.stdout}"
        assert "NOT READY" in result.stdout
        assert "0 decision(s)" in result.stdout


def test_flexible_pool_counts_its_own_decisions_and_exact_playback_identity() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        _activate_pool_with_decision(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [
                sys.executable,
                str(GATE),
                "--db",
                str(db_path),
                "--exe",
                str(exe),
                "--since",
                "2023-01-01 00:00:00",
                "--min-decisions",
                "1",
                "--min-reviewers",
                "1",
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert "review mode: flexible-pool" in result.stdout
        # One canonical first opinion (review_events, the paid path) plus one pool second opinion.
        assert "phone decisions in window: 2" in result.stdout
        assert "all 2 decision(s) carry a receipt" in result.stdout


def test_flexible_pool_refuses_a_decision_from_another_playback_guard() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        _activate_pool_with_decision(db_path, guard="obsolete-guard")
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [
                sys.executable,
                str(GATE),
                "--db",
                str(db_path),
                "--exe",
                str(exe),
                "--since",
                "2023-01-01 00:00:00",
                "--min-decisions",
                "1",
                "--min-reviewers",
                "1",
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 1
        assert "obsolete-guard" in result.stdout


def test_the_couch_policy_version_matches_the_rust_constant() -> None:
    """The phone writes policy-4 receipts (`COUCH_PLAYBACK_POLICY_VERSION`); the gate must know them.

    Measured 2026-09-04 on the live library: 1,104 of 1,812 receipts were policy 4 and every one was
    reported as violating the contract, because this gate only knew policies 1-3.
    """
    match = re.search(
        r"const COUCH_PLAYBACK_POLICY_VERSION:\s*i64\s*=\s*(\d+);",
        couch_surface(REPO_ROOT / "src-tauri" / "src"),
    )
    assert match, "COUCH_PLAYBACK_POLICY_VERSION is gone from couch.rs"
    assert int(match.group(1)) == gate.COUCH_PLAYBACK_POLICY_VERSION == 4


def test_flexible_pool_counts_canonical_first_opinions_from_review_events() -> None:
    """A pool clip nobody has judged is decided through the PAID canonical path (`review_events`).

    `review_pool_decisions` holds only second opinions. Measured 2026-09-04: 1,042 phone decisions
    since the live build and zero pool rows, and the gate said "0 decision(s)" — a window full of
    evidence read as empty.
    """
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        _activate_pool_with_decision(db_path, with_pool_decision=False)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2023-01-01 00:00:00", "--min-decisions", "1", "--min-reviewers", "1"],
            capture_output=True, text=True,
        )
        assert "review mode: flexible-pool" in result.stdout
        assert "phone decisions in window: 1" in result.stdout, result.stdout
        assert result.returncode == 0, result.stdout + result.stderr
        assert "all 1 decision(s) carry a receipt" in result.stdout


def test_policy_four_receipt_evidences_a_decision_only_with_the_exact_span() -> None:
    for source_end_ms, ready in ((1000, True), (1500, False)):
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            db_path = tmp / "t.db"
            _seed(db_path)
            conn = sqlite3.connect(db_path)
            _insert_receipt(
                conn, "s1", 0, CONTENT_HASH, 0.97, "2026-08-18 21:19:20", "Sara",
                policy_version=gate.COUCH_PLAYBACK_POLICY_VERSION, source_end_ms=source_end_ms,
            )
            conn.commit()
            conn.close()
            exe = tmp / "cortex-speech-app.exe"
            exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
            result = subprocess.run(
                [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
                 "--since", "2020-01-01 00:00:00", "--min-decisions", "1", "--min-reviewers", "1"],
                capture_output=True, text=True,
            )
            if ready:
                assert result.returncode == 0, f"a policy-4 receipt with the exact span is evidence:\n{result.stdout}"
                assert "all 1 decision(s) carry a receipt" in result.stdout
            else:
                assert result.returncode == 1, result.stdout
                assert "FAIL [receipt integrity]" in result.stdout, result.stdout


def test_verify_10_keeps_the_current_build_empty_canary_red() -> None:
    """The real aggregator must run this proof, without a skip probe or a backdated window.

    The gate's own empty-window test is necessary but insufficient: before this regression it could
    refuse perfectly while verify-10 never called it, allowing the full sweep to become green with
    0/20 current-build decisions. Drive the command registered in ``GATES`` through verify-10's real
    command runner against an isolated database and binary. The executable mtime supplies the default
    deployment cutoff; no ``--since`` is permitted in the production command or this test.
    """
    spec = importlib.util.spec_from_file_location("verify_10_playback_policy", VERIFY_10)
    assert spec and spec.loader
    verify = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = verify
    spec.loader.exec_module(verify)

    matches = [entry for entry in verify.GATES if entry[0] == "playback-enforcement-readiness"]
    assert len(matches) == 1, "verify-10 must register exactly one deployed playback canary"
    name, tier, kind, payload, cwd, probe, _charter = matches[0]
    assert tier == 2 and kind == "cmd", "the canary belongs to the live/deployed-binary tier"
    assert cwd == verify.APP
    assert probe is None, "missing evidence is RED; an env probe must not skip the canary"
    assert str(GATE) in payload and "--active-release" in payload
    assert str(verify.EXE) not in payload
    assert "--since" not in payload, "verify-10 must use the exact binary's own build-time cutoff"
    assert "--min-decisions" not in payload, "the gate's pinned 20-decision default must remain in force"

    if os.name != "nt":
        # Everything above is the platform-independent half — how the canary is REGISTERED — and it
        # just ran. What follows drives it through `verify.run_gate`, and that enters verify-10's
        # live product authority, which is Windows Known Folder resolution end to end. It cannot
        # exist on a Linux/macOS runner, so the run half is announced as skipped rather than being
        # handed a fabricated authority. On Windows — where verify-10 actually certifies — the whole
        # case runs unchanged, so the 0/20-must-be-RED pin is never weakened where it counts.
        print("    SKIP: live-run half is Windows-only (Known Folder resolution); registration pins ran")
        return

    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        # The child never receives CORTEX_DB: the gate's environment allowlist admits only
        # CORTEX_APP_EXE, and live-authority mode pins APPDATA to the canonical roots. Seed the
        # database at the canonical location under a synthetic roaming root so every run audits
        # exactly this fixture.
        db_path = tmp / "live-roaming" / "cortex-speech" / "cortex-speech.db"
        db_path.parent.mkdir(parents=True)
        _seed(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")

        # Exercise the exact registered command through the aggregator, changing only the isolated
        # binary path. The old event predates the just-written executable, so the current-build
        # window is genuinely 0/20 without manufacturing or backdating any evidence.
        isolated_payload = payload.replace("--active-release", f'--exe "{exe}"')
        previous_db = os.environ.get("CORTEX_DB")
        previous_log_dir = verify.LOG_DIR
        # run_gate builds the child environment against the canonical live data roots. Resolving
        # them is real Windows behaviour (SHGetKnownFolderPath — deliberately not overridable by
        # env) and stays asserted here, but the roots themselves name a WORKSTATION's live library.
        # A hosted Windows runner has none, so the child there refused with "unable to open
        # database file" — a different, equally correct refusal that proves nothing about the 0/20
        # canary this test exists to keep red. Point the roots at the seeded fixture on every
        # platform so the proof is the same proof everywhere.
        if os.name == "nt":
            roaming, local = verify._canonical_live_data_roots()
            assert roaming.is_dir() and local.is_dir(), (roaming, local)
        live_roots = mock.patch.object(
            verify,
            "_canonical_live_data_roots",
            return_value=(tmp / "live-roaming", tmp / "live-local"),
        )
        try:
            os.environ["CORTEX_DB"] = str(db_path)
            verify.LOG_DIR = tmp / "verify-logs"
            with live_roots:
                status, _seconds, detail = verify.run_gate(name, kind, isolated_payload, cwd, probe, timeout=30)
        finally:
            verify.LOG_DIR = previous_log_dir
            if previous_db is None:
                os.environ.pop("CORTEX_DB", None)
            else:
                os.environ["CORTEX_DB"] = previous_db

        assert status == verify.FAIL, f"verify-10 must be RED on a 0/20 current-build canary:\n{detail}"
        assert "0 decision(s)" in detail and "need 20" in detail, detail


def test_a_binary_that_cannot_enforce_is_refused_however_quiet_the_log() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"a binary built before the guard existed")
        result = _run(db_path, exe, "2020-01-01 00:00:00")
        assert result.returncode == 1, f"silence from a build without the guard is not evidence:\n{result.stdout}"
        assert "vacuous" in result.stdout


def test_a_covered_decision_passes_so_the_gate_is_not_merely_a_refuser() -> None:
    """A gate that always says NO is as useless as one that always says YES."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        # TWO reviewers, because the gate's device bar is part of what "ready" means. A positive
        # control that dodges one of the checks is not a control for the gate as configured.
        conn.execute(
            """INSERT INTO speech_segments VALUES
                   (?, 1, '525252', ?, 1000, '{"source_start_ms":0,"source_end_ms":1000}')""",
            ("s2", OTHER_CONTENT_HASH),
        )
        _insert_event(conn, "s2", "Hemn", "edit", "2026-08-20 10:00:00", decision_revision=1)
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.97, "2026-08-18 21:19:20", "Sara")
        _insert_receipt(conn, "s2", 0, OTHER_CONTENT_HASH, 0.93, "2026-08-20 10:00:00", "Hemn")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 0, f"a covered decision must read as ready:\n{result.stdout}"
        assert "READY" in result.stdout


def test_a_receipt_below_the_bar_is_reported_as_a_refusal() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.42, "2026-08-18 21:19:20", "Sara")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 1
        assert "enforcement REFUSED 1 of 1" in result.stdout, result.stdout


def test_one_device_is_not_enough_to_enforce_on_eight() -> None:
    """Coverage from a single phone is not evidence about the other seven reviewers' browsers."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.99, "2026-08-18 21:19:20", "Sara")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(b"\x00" + gate.ENFORCE_MARKER + b"\x00")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 1, "one reviewer must not clear the device bar: " + result.stdout
        assert "only 1 reviewer(s)" in result.stdout, result.stdout


def test_iso_cutoffs_count_the_same_canonical_and_pool_decisions() -> None:
    """The advertised T/Z format must not hide real work or disagree across namespaces."""
    with sqlite3.connect(":memory:") as conn:
        conn.executescript("""
            CREATE TABLE review_events(id, segment_id, reviewer, created_at, timestamp_ms, source, action);
            INSERT INTO review_events VALUES
                (1, 'before', 'reviewer-a', '2026-09-04 20:27:46', 1788553666000, 'couch', 'accept'),
                (2, 'boundary', 'reviewer-a', '2026-09-04 20:27:47', 1788553667000, 'couch', 'accept'),
                (3, 'later', 'reviewer-b', '2026-09-04 20:27:48', 1788553668000, 'couch', 'accept');
            CREATE TABLE effective_review_pool_decisions_v62(
                id, segment_id, reviewer, created_at_ms, served_revision, audio_content_hash,
                source_start_ms, source_end_ms, duration_ms, playback_guard_version, action);
            INSERT INTO effective_review_pool_decisions_v62
                SELECT id, segment_id, reviewer, timestamp_ms, 1, 'hash', 0, 1000, 1000, 'guard', action
                FROM review_events;
        """)
        for cutoff in (
            '2026-09-04 20:27:47', '2026-09-04T20:27:47Z',
            '2026-09-04T23:27:47+03:00', '2026-09-04T15:27:47-05:00',
            '2026-09-05T00:27:47+04:00',
        ):
            assert [row[0] for row in gate.decisions_since(conn, cutoff)] == [2, 3], cutoff
            assert [row[0] for row in gate.pool_decisions_since(conn, cutoff)] == [2, 3], cutoff
        for cutoff in ('2026-09-04T20:27:47.000001Z', '2026-09-04T23:27:47.500000+03:00'):
            assert [row[0] for row in gate.decisions_since(conn, cutoff)] == [3], cutoff
            assert [row[0] for row in gate.pool_decisions_since(conn, cutoff)] == [3], cutoff


def test_invalid_cutoffs_fail_before_reading_any_database() -> None:
    for cutoff in ('invalid', '2026-09-04', '2026-02-30T20:27:47Z'):
        result = subprocess.run(
            [sys.executable, str(GATE), '--since', cutoff], capture_output=True, text=True,
        )
        assert result.returncode == 2, result.stdout + result.stderr
        assert 'cutoff must be an ISO date and time' in result.stderr
        assert 'PLAYBACK ENFORCEMENT READINESS' not in result.stdout


def test_the_default_window_is_utc_like_the_rows_it_filters() -> None:
    """A local-time cutoff against UTC rows hides exactly the most recent work.

    Measured 2026-08-19 on a UTC+3 machine while a reviewer was mid-session: the gate derived its
    cutoff from the exe mtime in local time and compared it to SQLite `datetime('now')` values, which
    are UTC. It reported 0 decisions against 9 real ones -- and concealed the first genuine
    below-bar receipt the observe mode existed to surface.
    """
    import datetime as dt

    with tempfile.TemporaryDirectory() as raw:
        exe = Path(raw) / "cortex-speech-app.exe"
        exe.write_bytes(gate.ENFORCE_MARKER)
        mtime = exe.stat().st_mtime
        expected = dt.datetime.fromtimestamp(mtime, dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(Path(raw) / "missing.db"), "--exe", str(exe)],
            capture_output=True, text=True,
        )
        assert f"decisions at or after {expected}" in result.stdout, (
            "the default window must be UTC, matching the timestamps in the database: " + result.stdout
        )


def test_a_decision_that_bumped_the_revision_still_counts_as_evidenced() -> None:
    """The decision advances the segment's revision, and the receipt stays behind it.

    Measured 2026-08-19 against the live library: nine correctly-guarded decisions (0.959, 0.900,
    0.947 among them) were reported as having "no receipt", because the gate looked up the segment's
    CURRENT revision while every receipt sat at the revision in force when the guard actually ran.
    A gate that reads NOT READY however well the system behaves teaches the owner to ignore it, which
    is worse than not having one.
    """
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        # The decision landed at revision 3 and pushed the row to 4 -- exactly what the server does.
        conn.execute("UPDATE speech_segments SET review_revision = 4 WHERE id = 's1'")
        conn.execute("UPDATE review_compensation_ledger SET decision_revision = 4 WHERE review_event_id = 1")
        _insert_receipt(conn, "s1", 3, CONTENT_HASH, 0.96, "2026-08-18 21:19:20", "Sara")
        conn.execute(
            """INSERT INTO speech_segments VALUES
                   (?, 4, '525252', ?, 1000, '{"source_start_ms":0,"source_end_ms":1000}')""",
            ("s2", OTHER_CONTENT_HASH),
        )
        _insert_event(conn, "s2", "Hemn", "edit", "2026-08-18 21:19:20", decision_revision=4)
        _insert_receipt(conn, "s2", 3, OTHER_CONTENT_HASH, 0.93, "2026-08-18 21:19:20", "Hemn")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(gate.ENFORCE_MARKER)
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert "carry a receipt at or above the bar" in result.stdout, (
            "a receipt one revision behind its own decision is still that decision's evidence: "
            + result.stdout
        )
        assert result.returncode == 0, result.stdout


def test_anothers_receipt_does_not_evidence_my_decision() -> None:
    """Listening is personal in the gate exactly as in the backend guard.

    Found by the hunt: matching receipts on segment+time alone let reviewer A's listen evidence
    reviewer B's blind verdict. The gate mirrors db::has_sufficient_playback_evidence, so it must
    mirror the reviewer scoping too or it reports READY for a system the guard would refuse.
    """
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        # The receipt at the decision's own second — but minted by SOMEBODY ELSE.
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.99, "2026-08-18 21:19:20", "Hemn")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(gate.ENFORCE_MARKER)
        result = subprocess.run(
            [sys.executable, str(GATE), "--db", str(db_path), "--exe", str(exe),
             "--since", "2020-01-01 00:00:00", "--min-decisions", "1"],
            capture_output=True, text=True,
        )
        assert result.returncode == 1, "someone else's listen must not read as this reviewer's evidence: " + result.stdout
        assert "enforcement REFUSED 1 of 1" in result.stdout, result.stdout


def test_another_audio_content_hash_cannot_evidence_the_current_bytes() -> None:
    """Revision/reviewer equality is not audio identity.

    A high-coverage receipt for stale bytes must not hide a below-bar receipt for the segment's
    current server-owned content hash. Rust binds all four fields; the retrospective gate must too.
    """
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 0.42, "2026-08-18 21:19:20", "Sara")
        _insert_receipt(conn, "s1", 0, OTHER_CONTENT_HASH, 1.0, "2026-08-18 21:19:20", "Sara")
        conn.commit()
        conn.close()
        exe = tmp / "cortex-speech-app.exe"
        exe.write_bytes(gate.ENFORCE_MARKER)
        result = subprocess.run(
            [
                sys.executable,
                str(GATE),
                "--db",
                str(db_path),
                "--exe",
                str(exe),
                "--since",
                "2020-01-01 00:00:00",
                "--min-decisions",
                "1",
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 1, result.stdout
        assert "best canonical coverage 0.42" in result.stdout, result.stdout


def test_stored_ratio_cannot_override_zero_raw_listening_or_wrong_policy() -> None:
    """The durable authority is raw media time under the current policy, not a mutable REAL."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(
            conn,
            "s1",
            0,
            CONTENT_HASH,
            1.0,
            "2026-08-18 21:19:20",
            "Sara",
            played_ms=0,
            policy_version=999,
        )
        conn.commit()
        reason = gate.uncovered(conn, "s1", "2026-08-18 21:19:20", "Sara", 0, EVENT_TIMESTAMP_MS)
        audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
        conn.close()
        assert audited == 1
        assert reason is not None and "best canonical coverage 0.00" in reason
        assert any("policy_version=999" in error for error in semantic_errors)


def test_legacy_policy_one_receipt_is_historical_but_never_authorizes() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(
            conn,
            "s1",
            0,
            "424242",
            1.0,
            "2026-08-18 21:19:20",
            "Sara",
            policy_version=gate.LEGACY_PLAYBACK_POLICY_VERSION,
            omit_source_span=True,
        )
        conn.commit()

        audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
        reason = gate.uncovered(
            conn,
            "s1",
            "2026-08-18 21:19:20",
            "Sara",
            0,
            EVENT_TIMESTAMP_MS,
        )
        conn.close()

        assert audited == 1 and semantic_errors == []
        assert reason is not None and "best canonical coverage 0.00" in reason


def test_policy_two_content_hash_receipt_is_historical_but_never_authorizes_v3() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(
            conn,
            "s1",
            0,
            CONTENT_HASH,
            1.0,
            "2026-08-18 21:19:20",
            "Sara",
            policy_version=gate.CONTENT_HASH_ONLY_PLAYBACK_POLICY_VERSION,
            omit_source_span=True,
        )
        conn.commit()

        audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
        reason = gate.uncovered(
            conn,
            "s1",
            "2026-08-18 21:19:20",
            "Sara",
            0,
            EVENT_TIMESTAMP_MS,
        )
        conn.close()

        assert audited == 1 and semantic_errors == []
        assert reason is not None and "best canonical coverage 0.00" in reason


def test_policy_three_receipt_span_must_exactly_match_even_when_duration_matches() -> None:
    cases = [
        (None, None, True, "coordinates are not exact integers"),
        ("zero", 1000, False, "coordinates are not exact integers"),
        (0, 0, False, "not a non-empty forward range"),
        (1000, 2000, False, "disagrees with server-owned (0, 1000)"),
    ]
    for start, end, omit, expected in cases:
        with tempfile.TemporaryDirectory() as raw:
            db_path = Path(raw) / "t.db"
            _seed(db_path)
            conn = sqlite3.connect(db_path)
            _insert_receipt(
                conn,
                "s1",
                0,
                CONTENT_HASH,
                1.0,
                "2026-08-18 21:19:20",
                "Sara",
                source_start_ms=start,  # type: ignore[arg-type]
                source_end_ms=end,
                omit_source_span=omit,
            )
            conn.commit()
            audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
            reason = gate.uncovered(
                conn,
                "s1",
                "2026-08-18 21:19:20",
                "Sara",
                0,
                EVENT_TIMESTAMP_MS,
            )
            conn.close()
            assert audited == 1 and any(expected in error for error in semantic_errors), semantic_errors
            assert reason is not None and expected in reason, reason


def test_policy_three_allows_one_ms_endpoint_rounding_but_refuses_tenfold_duration() -> None:
    for duration_ms, should_pass in ((1001, True), (10_000, False)):
        with tempfile.TemporaryDirectory() as raw:
            db_path = Path(raw) / "t.db"
            _seed(db_path)
            conn = sqlite3.connect(db_path)
            conn.execute("UPDATE speech_segments SET duration_ms=? WHERE id='s1'", (duration_ms,))
            _insert_receipt(
                conn,
                "s1",
                0,
                CONTENT_HASH,
                1.0,
                "2026-08-18 21:19:20",
                "Sara",
                duration_ms=duration_ms,
                source_start_ms=0,
                source_end_ms=1000,
            )
            conn.commit()
            audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
            reason = gate.uncovered(
                conn,
                "s1",
                "2026-08-18 21:19:20",
                "Sara",
                0,
                EVENT_TIMESTAMP_MS,
            )
            conn.close()

            assert audited == 1
            if should_pass:
                assert semantic_errors == []
                assert reason is None
            else:
                assert any("differs from exact source span length" in item for item in semantic_errors)
                assert reason is not None and "differs from exact source span length" in reason


def test_policy_three_refuses_null_or_malformed_server_owned_alignment_span() -> None:
    cases = [
        (None, "alignment_json is not text"),
        ("{", "alignment_json is malformed"),
        ('{"source_start_ms":true,"source_end_ms":1000}', "not exact integers"),
        ('{"source_start_ms":1000,"source_end_ms":1000}', "not a non-empty forward range"),
    ]
    for alignment_json, expected in cases:
        with tempfile.TemporaryDirectory() as raw:
            db_path = Path(raw) / "t.db"
            _seed(db_path)
            conn = sqlite3.connect(db_path)
            conn.execute("UPDATE speech_segments SET alignment_json=? WHERE id='s1'", (alignment_json,))
            _insert_receipt(conn, "s1", 0, CONTENT_HASH, 1.0, "2026-08-18 21:19:20", "Sara")
            conn.commit()
            audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
            reason = gate.uncovered(
                conn,
                "s1",
                "2026-08-18 21:19:20",
                "Sara",
                0,
                EVENT_TIMESTAMP_MS,
            )
            conn.close()
            assert audited == 1 and any(expected in error for error in semantic_errors), semantic_errors
            assert reason is not None and expected in reason, reason


def test_blank_server_content_hash_cannot_be_replaced_by_a_client_claim() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 's1'")
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 1.0, "2026-08-18 21:19:20", "Sara")
        conn.commit()

        reason = gate.uncovered(conn, "s1", "2026-08-18 21:19:20", "Sara", 0, EVENT_TIMESTAMP_MS)
        audited, semantic_errors = gate.playback_receipt_semantic_issues(conn)
        conn.close()

        assert reason is not None and "no canonical server-derived audio content hash" in reason
        assert audited == 1
        assert any("no canonical server-derived audio content hash" in error for error in semantic_errors)


def test_receipt_minted_after_the_decision_cannot_retroactively_authorize_it() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        db_path = tmp / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 1.0, "2026-08-18 21:19:25", "Sara")
        conn.commit()
        reason = gate.uncovered(conn, "s1", "2026-08-18 21:19:20", "Sara", 0, EVENT_TIMESTAMP_MS)
        conn.close()
        assert reason is not None and "best canonical coverage 0.00" in reason


def test_old_revision_cannot_authorize_a_later_decision_with_the_same_audio() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute("UPDATE speech_segments SET review_revision = 5 WHERE id = 's1'")
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 1.0, "2026-08-18 21:19:19", "Sara")
        conn.commit()
        reason = gate.uncovered(conn, "s1", "2026-08-18 21:19:20", "Sara", 4, EVENT_TIMESTAMP_MS)
        conn.close()
        assert reason is not None and "revision 4 best canonical coverage 0.00" in reason


def test_ledger_identity_mismatch_cannot_name_a_receipt_revision() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute("UPDATE review_compensation_ledger SET reviewer = 'Hemn'")
        _insert_receipt(conn, "s1", 0, CONTENT_HASH, 1.0, "2026-08-18 21:19:20", "Sara")
        conn.commit()
        revision, reason = gate.corpus_receipt_revision_for_event(conn, 1, "s1", "Sara", "couch")
        conn.close()
        assert revision is None and reason is not None and "identity disagree" in reason


def test_multiple_ledger_rows_cannot_name_a_receipt_revision() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute(
            """INSERT INTO review_compensation_ledger VALUES
                 (1, 'other-policy', 1, 's1', 'Sara', 'couch')"""
        )
        revision, reason = gate.corpus_receipt_revision_for_event(conn, 1, "s1", "Sara", "couch")
        conn.close()
        assert revision is None and reason is not None and "2 immutable compensation rows" in reason


def test_same_second_receipt_started_after_event_is_not_evidence() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db_path = Path(raw) / "t.db"
        _seed(db_path)
        conn = sqlite3.connect(db_path)
        _insert_receipt(
            conn,
            "s1",
            0,
            CONTENT_HASH,
            1.0,
            "2026-08-18 21:19:20",
            "Sara",
            started_at_ms=EVENT_TIMESTAMP_MS + 1,
        )
        conn.commit()
        reason = gate.uncovered(conn, "s1", "2026-08-18 21:19:20", "Sara", 0, EVENT_TIMESTAMP_MS)
        conn.close()
        assert reason is not None and "after decision" in reason


def test_legacy_repair_requires_exact_current_revision_and_decision_identity() -> None:
    conn = _operational_connection(cutoff=10)
    conn.execute(
        """INSERT INTO speech_segments
               (id, review_revision, audio_fingerprint, audio_content_hash, duration_ms,
                verified, human_decision,
                verdict, corrected_at, reviewed_by, annotated_transcript, escalated)
             VALUES ('legacy', 2, '424242',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     1000, 0, 'edit', 'human_edit',
                     '2026-08-18 21:19:20', NULL, 'correct text', 0)"""
    )
    conn.execute(
        """INSERT INTO decision_log
               (segment_id, decision_type, timestamp_ms, human_decision, created_at)
             VALUES ('legacy', 'edit', ?, 'edit', '2026-08-18 21:19:20')""",
        (EVENT_TIMESTAMP_MS,),
    )
    _insert_receipt(conn, "legacy", 1, CONTENT_HASH, 1.0, "2026-08-18 21:19:20", None)
    finalizable, ambiguous = repair.legacy_finalizable_reviews(conn)
    assert [row["segment_id"] for row in finalizable] == ["legacy"] and not ambiguous

    # A later text/alignment revision must not be rebound to the old receipt.
    conn.execute("UPDATE speech_segments SET review_revision = 5 WHERE id = 'legacy'")
    finalizable, ambiguous = repair.legacy_finalizable_reviews(conn)
    conn.close()
    assert not finalizable
    assert len(ambiguous) == 1 and "revision 4 best canonical coverage 0.00" in ambiguous[0]["failure"]


def test_requeue_refuses_a_later_desktop_decision_even_if_old_couch_event_is_latest() -> None:
    conn = _operational_connection(cutoff=1)
    conn.execute(
        """INSERT INTO speech_segments
               (id, review_revision, audio_fingerprint, audio_content_hash, duration_ms,
                verified, human_decision,
                verdict, corrected_at, reviewed_by, annotated_transcript, escalated)
             VALUES ('changed', 2, '424242',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     1000, 1, 'edit', 'human_edit',
                     '2026-08-18 21:20:00', NULL, 'desktop correction', 0)"""
    )
    conn.execute(
        """INSERT INTO review_events
               (segment_id, reviewer, action, source, created_at, timestamp_ms)
             VALUES ('changed', 'Sara', 'accept', 'couch', '2026-08-18 21:19:20', ?)""",
        (EVENT_TIMESTAMP_MS,),
    )
    conn.execute(
        """INSERT INTO decision_log
               (segment_id, decision_type, timestamp_ms, human_decision, created_at)
             VALUES ('changed', 'accept', ?, 'accept', '2026-08-18 21:19:20'),
                    ('changed', 'edit', ?, 'edit', '2026-08-18 21:20:00')""",
        (EVENT_TIMESTAMP_MS, EVENT_TIMESTAMP_MS + 40_000),
    )
    targets, blocked = requeue.audit_requeue_candidates(conn, "2020-01-01 00:00:00")
    conn.close()
    assert not targets and len(blocked) == 1
    assert "current decision/action disagree" in blocked[0]["evidence_failure"]


def test_requeue_reports_malformed_event_identity_without_crashing() -> None:
    conn = _operational_connection(cutoff=1)
    conn.execute(
        """INSERT INTO speech_segments
               (id, review_revision, audio_fingerprint, audio_content_hash, duration_ms,
                verified, human_decision,
                verdict, corrected_at, reviewed_by, annotated_transcript, escalated)
             VALUES ('malformed', 1, '424242',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     1000, 1, 'accept', 'human_accept',
                     '2026-08-18 21:19:20', 'Sara', 'text', 0)"""
    )
    conn.execute(
        """INSERT INTO review_events
               (segment_id, reviewer, action, source, created_at, timestamp_ms)
             VALUES ('malformed', ?, 'accept', 'couch', '2026-08-18 21:19:20', ?)""",
        (sqlite3.Binary(b"Sara"), EVENT_TIMESTAMP_MS),
    )
    targets, blocked = requeue.audit_requeue_candidates(conn, "2020-01-01 00:00:00")
    conn.close()
    assert not targets and len(blocked) == 1
    assert "reviewer identity" in blocked[0]["evidence_failure"]


def test_requeue_never_clears_post_policy_paid_state_without_compensation_reversal() -> None:
    conn = _operational_connection(cutoff=0)
    conn.execute(
        """INSERT INTO speech_segments
               (id, review_revision, audio_fingerprint, audio_content_hash, duration_ms,
                verified, human_decision,
                verdict, corrected_at, reviewed_by, annotated_transcript, escalated)
             VALUES ('paid', 1, '424242',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     1000, 1, 'accept', 'human_accept',
                     '2026-08-18 21:19:20', 'Sara', 'text', 0)"""
    )
    conn.execute(
        """INSERT INTO review_events
               (segment_id, reviewer, action, source, created_at, timestamp_ms)
             VALUES ('paid', 'Sara', 'accept', 'couch', '2026-08-18 21:19:20', ?)""",
        (EVENT_TIMESTAMP_MS,),
    )
    conn.execute(
        """INSERT INTO decision_log
               (segment_id, decision_type, timestamp_ms, human_decision, created_at)
             VALUES ('paid', 'accept', ?, 'accept', '2026-08-18 21:19:20')""",
        (EVENT_TIMESTAMP_MS,),
    )
    conn.execute(
        """INSERT INTO review_compensation_ledger VALUES
             (1, 'review-iqd-v1-2026-08-21', 1, 'paid', 'Sara', 'couch')"""
    )
    targets, blocked = requeue.audit_requeue_candidates(conn, "2020-01-01 00:00:00")
    conn.close()
    assert not targets and len(blocked) == 1
    assert "compensation reversal" in blocked[0]["evidence_failure"]


def test_requeue_can_select_only_an_exact_unpaid_legacy_current_decision() -> None:
    conn = _operational_connection(cutoff=1)
    conn.execute(
        """INSERT INTO speech_segments
               (id, review_revision, audio_fingerprint, audio_content_hash, duration_ms,
                verified, human_decision,
                verdict, corrected_at, reviewed_by, annotated_transcript, escalated)
             VALUES ('legacy', 1, '424242',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     1000, 1, 'reject', 'human_reject',
                     '2026-08-18 21:19:20', 'Sara', 'kept text', 0)"""
    )
    conn.execute(
        """INSERT INTO review_events
               (segment_id, reviewer, action, source, created_at, timestamp_ms)
             VALUES ('legacy', 'Sara', 'reject', 'couch', '2026-08-18 21:19:20', ?)""",
        (EVENT_TIMESTAMP_MS,),
    )
    conn.execute(
        """INSERT INTO decision_log
               (segment_id, decision_type, timestamp_ms, human_decision, created_at)
             VALUES ('legacy', 'reject', ?, 'reject', '2026-08-18 21:19:20')""",
        (EVENT_TIMESTAMP_MS,),
    )
    targets, blocked = requeue.audit_requeue_candidates(conn, "2020-01-01 00:00:00")
    conn.close()
    assert not blocked and [row["segment_id"] for row in targets] == ["legacy"]


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"PLAYBACK ENFORCEMENT READINESS POLICY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    sys.exit(main())
