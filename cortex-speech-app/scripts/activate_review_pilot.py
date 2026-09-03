#!/usr/bin/env python3
"""Atomically arm the strict Rubar and Alle paid-review certification pilot.

This is the only supported writer for ``review_pilot_policy.json``. It requires Couch, the desktop
app, importer, and watchdog to be offline; takes SQLite's write reservation; compares the caller's
event-id precondition; revokes auto-resume; backs up the remembered session/policy; narrows durable
pairing, cookie, and outstanding hidden-check state to Rubar + Alle without decrypting or changing
their DPAPI tokens; writes the same policy snapshot into the session; rechecks the event-id; then
promotes the policy, session, and optional focus together. The revocation marker is removed last.
Any crash after mutation begins therefore
leaves Couch unable to resume, never unrestricted.

For a pre-v59 library, establish durable revocation before the offline maintenance migration. Once
the current schema and foreign keys are clean, inspect and activate with the printed event id::

    python scripts/activate_review_pilot.py --prepare-maintenance-revocation
    python scripts/activate_review_pilot.py --inspect
    python scripts/activate_review_pilot.py --expected-max-review-event-id 863

Do not hand-edit the policy file. A replacement additionally requires its current SHA-256 via
``--expected-policy-sha256``.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
import uuid
from pathlib import Path

from check_database_integrity import DEFAULT_MIGRATIONS, latest_source_schema, source_migrations
from check_reviewer_links_live import strict_json_loads, validate_saved_session_shape
from pilot_focus_contract import (
    VOICE_FOCUS_FILE,
    focus_evidence,
    load_pilot_focus_contract,
    load_voice_focus_ids,
    verify_controlled_pilot_focus,
)
from review_pilot_hidden_contract import (
    HIDDEN_KEYS_PER_REVIEWER,
    HIDDEN_TABLE,
    TOTAL_HIDDEN_KEYS,
    audit_hidden_schema,
    parse_policy as parse_hidden_policy,
    policy_sha256 as hidden_policy_sha256,
)

POLICY_FILE = "review_pilot_policy.json"
SESSION_FILE = "couch_session.json"
REVOCATION_FILE = "couch_session.revoked"
POLICY_VERSION = "review-iqd-v1-2026-08-21"
REVIEWERS = ("Rubar", "Alle")
CAP_PER_REVIEWER = 10
TOTAL_CAP = 20
HIDDEN_QC_PER_REVIEWER = 2
TOTAL_HIDDEN_QC = len(REVIEWERS) * HIDDEN_QC_PER_REVIEWER
MAX_COMPENSATED_UI_ACTIONS = TOTAL_CAP + TOTAL_HIDDEN_QC
COUCH_PORT = 8737
# Derive this from the append-only Rust catalog. A copied integer silently drifted when v67 was
# added and made every otherwise-current pilot activation fail with a contradictory `schema 67/66`.
REQUIRED_SCHEMA = latest_source_schema(DEFAULT_MIGRATIONS)


from policy_python import sha256_file


def default_data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if not appdata:
        raise RuntimeError("APPDATA is unavailable; pass --data-dir")
    return Path(appdata) / "cortex-speech"


def atomic_write(path: Path, payload: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.tmp.{os.getpid()}.{uuid.uuid4().hex}")
    try:
        with temp.open("xb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp, path)
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


class CortexInstanceLock:
    """Exclusive owner of the same ``cortex.lock`` used by the GUI and importer."""

    def __init__(self, path: Path, handle: object) -> None:
        self.path = path
        self.handle = handle

    def close(self) -> None:
        if self.handle is None:
            return
        handle, self.handle = self.handle, None
        if os.name == "nt":
            import ctypes

            ctypes.windll.kernel32.CloseHandle(handle)
        else:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
            handle.close()
        try:
            self.path.unlink()
        except FileNotFoundError:
            pass

    def __enter__(self) -> "CortexInstanceLock":
        return self

    def __exit__(self, *_error: object) -> None:
        self.close()


def acquire_cortex_lock(data_dir: Path) -> CortexInstanceLock:
    """Acquire the app/importer's cross-process lock, recovering only a provably stale file."""
    path = data_dir / "cortex.lock"
    data_dir.mkdir(parents=True, exist_ok=True)
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.windll.kernel32
        kernel32.CreateFileW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            ctypes.c_void_p,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        kernel32.CreateFileW.restype = wintypes.HANDLE
        invalid = wintypes.HANDLE(-1).value
        for attempt in range(5):
            try:
                path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                if attempt == 4:
                    raise RuntimeError(f"cannot acquire {path}; the app or importer still owns it")
                time.sleep(0.08)
                continue
            handle = kernel32.CreateFileW(
                str(path),
                0xC0000000,  # GENERIC_READ | GENERIC_WRITE
                0,           # share_mode(0), exactly as Rust's InstanceLock
                None,
                1,           # CREATE_NEW
                0x80,        # FILE_ATTRIBUTE_NORMAL
                None,
            )
            if handle != invalid:
                return CortexInstanceLock(path, handle)
            if attempt < 4:
                time.sleep(0.08)
        raise RuntimeError(f"cannot acquire {path}; the app or importer may still be running")

    import fcntl

    handle = path.open("a+b")
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        handle.close()
        raise RuntimeError(f"cannot acquire {path}; the app or importer may still be running") from error
    return CortexInstanceLock(path, handle)


def _windows_process_names() -> set[str]:
    if os.name != "nt":
        return set()
    output = subprocess.check_output(
        ["tasklist", "/FO", "CSV", "/NH"], text=True, encoding="utf-8", errors="replace"
    )
    return {row[0].strip().lower() for row in csv.reader(output.splitlines()) if row}


def require_runtime_offline() -> None:
    names = _windows_process_names()
    forbidden = sorted(names.intersection({"cortex-speech-app.exe", "batch_importer.exe"}))
    if forbidden:
        raise RuntimeError("activation requires the app/importer offline; running: " + ", ".join(forbidden))
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.25)
        if probe.connect_ex(("127.0.0.1", COUCH_PORT)) == 0:
            raise RuntimeError(f"activation requires Couch port {COUCH_PORT} offline")
    if os.name == "nt":
        try:
            state = subprocess.check_output(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "(Get-ScheduledTask -TaskName 'CortexWatchdog' -ErrorAction Stop).State.ToString()",
                ],
                text=True,
                stderr=subprocess.STDOUT,
                timeout=15,
            ).strip()
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"cannot prove CortexWatchdog is disabled: {error}") from error
        if state.lower() != "disabled":
            raise RuntimeError(f"activation requires CortexWatchdog disabled; current state is {state!r}")


def validate_pilot_policy(value: object, source: str = POLICY_FILE) -> dict[str, object]:
    """Validate exactly the typed JSON contract Rust deserializes; bool is never an integer here."""
    policy_keys = {"schema_version", "after_review_event_id", "max_total_corpus_actions", "reviewers"}
    if not isinstance(value, dict) or set(value) != policy_keys:
        raise RuntimeError(f"{source} fields do not exactly match the controlled-pilot contract")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise RuntimeError(f"{source} schema must be integer version 1")
    if type(value["after_review_event_id"]) is not int or value["after_review_event_id"] < 0:
        raise RuntimeError(f"{source} review-event baseline must be a non-negative integer")
    if type(value["max_total_corpus_actions"]) is not int or value["max_total_corpus_actions"] != TOTAL_CAP:
        raise RuntimeError(f"{source} must cap exactly {TOTAL_CAP} corpus actions")
    reviewers = value["reviewers"]
    if not isinstance(reviewers, list) or len(reviewers) != len(REVIEWERS):
        raise RuntimeError(f"{source} must contain exactly {len(REVIEWERS)} reviewer objects")
    expected = sorted((name.lower(), CAP_PER_REVIEWER) for name in REVIEWERS)
    actual: list[tuple[str, int]] = []
    for entry in reviewers:
        if not isinstance(entry, dict) or set(entry) != {"name", "max_corpus_actions"}:
            raise RuntimeError(f"{source} reviewer fields do not exactly match the server contract")
        name = entry["name"]
        cap = entry["max_corpus_actions"]
        if not isinstance(name, str) or not name.strip() or type(cap) is not int:
            raise RuntimeError(f"{source} reviewer values have invalid types")
        actual.append((name.strip().lower(), cap))
    if sorted(actual) != expected:
        raise RuntimeError(
            f"{source} must contain exactly {' and '.join(REVIEWERS)} at "
            f"{CAP_PER_REVIEWER} corpus actions each"
        )
    return value


def validate_replaceable_existing_policy(value: object, source: str) -> dict[str, object]:
    """Validate the old two-person policy without accepting its roster as the new canon."""
    keys = {"schema_version", "after_review_event_id", "max_total_corpus_actions", "reviewers"}
    if not isinstance(value, dict) or set(value) != keys:
        raise RuntimeError(f"{source} fields do not exactly match the controlled-pilot contract")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise RuntimeError(f"{source} schema must be integer version 1")
    baseline = value["after_review_event_id"]
    if type(baseline) is not int or baseline < 0:
        raise RuntimeError(f"{source} review-event baseline must be a non-negative integer")
    if type(value["max_total_corpus_actions"]) is not int or value["max_total_corpus_actions"] != TOTAL_CAP:
        raise RuntimeError(f"{source} must cap exactly {TOTAL_CAP} corpus actions")
    reviewers = value["reviewers"]
    if not isinstance(reviewers, list) or len(reviewers) != 2:
        raise RuntimeError(f"{source} must contain exactly two reviewer objects")
    names: set[str] = set()
    for entry in reviewers:
        if not isinstance(entry, dict) or set(entry) != {"name", "max_corpus_actions"}:
            raise RuntimeError(f"{source} reviewer fields do not exactly match the server contract")
        name = entry["name"]
        cap = entry["max_corpus_actions"]
        if not isinstance(name, str) or not name.strip() or type(cap) is not int or cap != CAP_PER_REVIEWER:
            raise RuntimeError(f"{source} reviewer values are invalid")
        names.add(name.strip().lower())
    if len(names) != 2:
        raise RuntimeError(f"{source} reviewer names must be distinct")
    return value


def require_pristine_roster_replacement(
    conn: sqlite3.Connection,
    existing_policy: dict[str, object],
    maximum: int,
) -> None:
    baseline = int(existing_policy["after_review_event_id"])
    if maximum != baseline:
        raise RuntimeError(
            f"reviewer roster replacement is forbidden after pilot activity: baseline={baseline}, max={maximum}"
        )
    protected_tables = (
        "review_pilot_hidden_keys",
        "review_compensation_ledger",
        "human_decision_effect_events",
        "human_decision_effect_reversals",
        "review_flag_effect_events",
        "review_flag_effect_reversals",
    )
    nonempty = {
        table: int(conn.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0])
        for table in protected_tables
    }
    nonempty = {table: count for table, count in nonempty.items() if count}
    if nonempty:
        raise RuntimeError(
            f"reviewer roster replacement is forbidden after durable pilot activity: {nonempty}"
        )


def pilot_policy(after_review_event_id: int) -> dict[str, object]:
    return validate_pilot_policy({
        "schema_version": 1,
        "after_review_event_id": after_review_event_id,
        "max_total_corpus_actions": TOTAL_CAP,
        # Rust sorts names while validating the standalone policy, then structurally compares it
        # with the policy embedded in the remembered session. Emit that canonical order here too.
        "reviewers": [
            {"name": name, "max_corpus_actions": CAP_PER_REVIEWER}
            for name in sorted(REVIEWERS, key=str.lower)
        ],
    })


def _canonical_path(path: Path) -> str:
    return os.path.normcase(os.path.abspath(path))


def narrow_session(session: dict[str, object], db_path: Path, policy: dict[str, object]) -> dict[str, object]:
    if not isinstance(session, dict):
        raise RuntimeError(f"{SESSION_FILE} root must be an object")
    validate_saved_session_shape(session)
    recorded_db = session.get("db_path")
    if not isinstance(recorded_db, str) or _canonical_path(Path(recorded_db)) != _canonical_path(db_path):
        raise RuntimeError(f"{SESSION_FILE} belongs to a different database")
    pairing = session.get("reviewers")
    if not isinstance(pairing, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in pairing.items()):
        raise RuntimeError(f"{SESSION_FILE} has invalid durable pairing state")
    targets = {name.lower(): name for name in REVIEWERS}
    kept_pairing = {token: name for token, name in pairing.items() if name.strip().lower() in targets}
    for target in REVIEWERS:
        matches = [name for name in kept_pairing.values() if name.strip().lower() == target.lower()]
        if len(matches) != 1:
            raise RuntimeError(f"{SESSION_FILE} must contain exactly one durable pairing token for {target}")

    sessions = session.get("sessions", [])
    if not isinstance(sessions, list) or not all(isinstance(entry, dict) for entry in sessions):
        raise RuntimeError(f"{SESSION_FILE} has invalid cookie-session state")
    kept_sessions = [
        entry for entry in sessions
        if isinstance(entry.get("reviewer"), str) and entry["reviewer"].strip().lower() in targets
    ]
    def filtered_checks(field: str) -> list[object]:
        checks = session.get(field, [])
        if not isinstance(checks, list):
            raise RuntimeError(f"{SESSION_FILE} has invalid {field} state")
        kept: list[object] = []
        for entry in checks:
            if not isinstance(entry, list) or len(entry) != 2 or not all(isinstance(value, str) for value in entry):
                raise RuntimeError(f"{SESSION_FILE} has invalid {field} entry")
            if entry[1].strip().lower() in targets:
                kept.append(entry)
        return kept

    pilot_checks = filtered_checks("pilot_spot_checks")
    remembered_policy_raw = session.get("pilot_policy")
    if pilot_checks:
        if remembered_policy_raw is None:
            raise RuntimeError(
                f"{SESSION_FILE} contains pilot hidden keys without the policy that authorized them"
            )
        remembered_policy = validate_pilot_policy(
            remembered_policy_raw, f"{SESSION_FILE} remembered pilot policy"
        )
        if remembered_policy != policy:
            raise RuntimeError(
                f"{SESSION_FILE} pilot hidden keys belong to a different policy generation"
            )

    narrowed = dict(session)  # preserve fields added by newer binaries; change only authorization state
    narrowed["reviewers"] = kept_pairing
    narrowed["sessions"] = kept_sessions
    narrowed["spot_checks"] = filtered_checks("spot_checks")
    # Missing/empty in a pre-pilot session is valid. Nonempty keys are retained only when their
    # remembered policy proves the exact generation that authorized them; v59 imports that mirror
    # into the database before serving, where it becomes the lifetime authority.
    narrowed["pilot_spot_checks"] = pilot_checks
    narrowed["pilot_policy"] = policy
    return narrowed


def database_preflight(conn: sqlite3.Connection) -> int:
    quick = [str(row[0]) for row in conn.execute("PRAGMA quick_check")]
    full = [str(row[0]) for row in conn.execute("PRAGMA integrity_check")]
    if quick != ["ok"] or full != ["ok"]:
        raise RuntimeError(f"database integrity is not clean: quick={quick!r}, full={full!r}")
    try:
        actual_migrations = [
            (int(version), str(description))
            for version, description in conn.execute(
                "SELECT version, description FROM schema_migrations ORDER BY version"
            )
        ]
        required_migrations = source_migrations(DEFAULT_MIGRATIONS)
    except (OSError, ValueError, sqlite3.Error) as error:
        raise RuntimeError(f"migration history cannot be proven: {error}") from error
    schema = actual_migrations[-1][0] if actual_migrations else 0
    if actual_migrations != required_migrations:
        actual_by_version = dict(actual_migrations)
        required_by_version = dict(required_migrations)
        missing = sorted(set(required_by_version) - set(actual_by_version))
        unknown = sorted(set(actual_by_version) - set(required_by_version))
        mismatched = sorted(
            version
            for version in set(actual_by_version) & set(required_by_version)
            if actual_by_version[version] != required_by_version[version]
        )
        raise RuntimeError(
            "migration history does not exactly equal this release: "
            f"schema {schema}/{REQUIRED_SCHEMA}, missing={missing}, unknown={unknown}, "
            f"descriptionMismatch={mismatched}"
        )
    foreign_keys = conn.execute("PRAGMA foreign_key_check").fetchall()
    if foreign_keys:
        raise RuntimeError(f"database has {len(foreign_keys)} foreign-key violation(s); pilot activation is refused")
    try:
        present = conn.execute(
            "SELECT EXISTS(SELECT 1 FROM review_compensation_policies WHERE policy_version = ?)",
            (POLICY_VERSION,),
        ).fetchone()[0]
    except sqlite3.Error as error:
        raise RuntimeError(f"compensation policy table is unavailable: {error}") from error
    if not present:
        raise RuntimeError(f"database has no immutable compensation policy {POLICY_VERSION}")
    return int(conn.execute("SELECT COALESCE(MAX(id), 0) FROM review_events").fetchone()[0])


def _canonical_hidden_reviewer(actual: object, source: str) -> str:
    if not isinstance(actual, str) or actual != actual.strip():
        raise RuntimeError(f"{source} contains an invalid reviewer")
    matches = [name for name in REVIEWERS if name.lower() == actual.lower()]
    if len(matches) != 1:
        raise RuntimeError(f"{source} contains unauthorized reviewer {actual!r}")
    return matches[0]


def _canonical_hidden_segment(actual: object, source: str) -> str:
    if (
        not isinstance(actual, str)
        or actual != actual.strip()
        or not actual
        or len(actual.encode("utf-8")) > 256
        or not all(char.isalnum() or char in "_-." for char in actual)
    ):
        raise RuntimeError(f"{source} contains an invalid segment id {actual!r}")
    return actual


def import_pilot_hidden_authority(
    conn: sqlite3.Connection,
    policy: dict[str, object],
    session: dict[str, object],
) -> dict[str, object]:
    """Durably bridge every unambiguous v58 hidden key into the active v59 namespace.

    The caller owns an IMMEDIATE transaction.  Existing grants, retained session assignments, and
    completed post-baseline hidden events are one lifetime set: no source may mint replacements for
    another, and no key is accepted unless its exact policy/reviewer/segment identity is provable.
    """

    parsed_policy = parse_hidden_policy(policy, "activation policy")
    digest = hidden_policy_sha256(parsed_policy)
    baseline = parsed_policy.after_review_event_id
    _schema_evidence, schema_errors = audit_hidden_schema(conn)
    if schema_errors:
        raise RuntimeError("; ".join(schema_errors))

    conflicting = int(
        conn.execute(
            f"""SELECT COUNT(*) FROM {HIDDEN_TABLE}
                 WHERE (policy_sha256 = ? OR after_review_event_id = ?)
                   AND NOT (policy_sha256 = ? AND after_review_event_id = ?)""",
            (digest, baseline, digest, baseline),
        ).fetchone()[0]
    )
    if conflicting:
        raise RuntimeError(
            f"{conflicting} hidden-key grant(s) disagree with the activation policy SHA/baseline"
        )

    durable = {name: set() for name in REVIEWERS}
    for actual_reviewer, actual_segment in conn.execute(
        f"""SELECT reviewer, segment_id FROM {HIDDEN_TABLE}
             WHERE policy_sha256 = ? AND after_review_event_id = ?
             ORDER BY reviewer COLLATE NOCASE, segment_id""",
        (digest, baseline),
    ):
        reviewer = _canonical_hidden_reviewer(actual_reviewer, "durable hidden-key authority")
        segment = _canonical_hidden_segment(actual_segment, "durable hidden-key authority")
        if segment in durable[reviewer]:
            raise RuntimeError(f"durable hidden-key authority duplicates {reviewer}/{segment}")
        durable[reviewer].add(segment)

    retained = {name: set() for name in REVIEWERS}
    entries = session.get("pilot_spot_checks", [])
    if not isinstance(entries, list):
        raise RuntimeError(f"{SESSION_FILE} has invalid pilot_spot_checks state")
    for entry in entries:
        if not isinstance(entry, list) or len(entry) != 2:
            raise RuntimeError(f"{SESSION_FILE} has an invalid pilot_spot_checks entry")
        segment = _canonical_hidden_segment(entry[0], f"{SESSION_FILE}.pilot_spot_checks")
        reviewer = _canonical_hidden_reviewer(entry[1], f"{SESSION_FILE}.pilot_spot_checks")
        if segment in retained[reviewer]:
            raise RuntimeError(f"{SESSION_FILE} duplicates hidden key {reviewer}/{segment}")
        retained[reviewer].add(segment)

    completed = {name: set() for name in REVIEWERS}
    for event_id, actual_segment, actual_reviewer, action in conn.execute(
        """SELECT id, segment_id, reviewer, action FROM review_events
             WHERE id > ? AND source = 'couch_spot_check'
             ORDER BY id""",
        (baseline,),
    ):
        reviewer = _canonical_hidden_reviewer(actual_reviewer, f"hidden event {event_id}")
        segment = _canonical_hidden_segment(actual_segment, f"hidden event {event_id}")
        if action not in {"accept", "edit", "reject", "skip"}:
            raise RuntimeError(f"hidden event {event_id} has invalid action {action!r}")
        if segment in completed[reviewer]:
            raise RuntimeError(f"hidden key {reviewer}/{segment} has more than one completion event")
        completed[reviewer].add(segment)

    complete = {
        name: durable[name] | retained[name] | completed[name]
        for name in REVIEWERS
    }
    for reviewer, keys in complete.items():
        if len(keys) > HIDDEN_KEYS_PER_REVIEWER:
            raise RuntimeError(
                f"hidden-key lifetime set exceeds the {HIDDEN_KEYS_PER_REVIEWER}-key quota for {reviewer}"
            )
    if sum(len(keys) for keys in complete.values()) > TOTAL_HIDDEN_KEYS:
        raise RuntimeError(f"hidden-key lifetime set exceeds the {TOTAL_HIDDEN_KEYS}-key global quota")

    inserted = 0
    for reviewer in REVIEWERS:
        for segment in sorted(complete[reviewer] - durable[reviewer]):
            conn.execute(
                f"""INSERT INTO {HIDDEN_TABLE}
                       (policy_sha256, after_review_event_id, reviewer, segment_id)
                     VALUES (?, ?, ?, ?)""",
                (digest, baseline, reviewer, segment),
            )
            inserted += 1

    verified = {name: set() for name in REVIEWERS}
    for actual_reviewer, actual_segment in conn.execute(
        f"""SELECT reviewer, segment_id FROM {HIDDEN_TABLE}
             WHERE policy_sha256 = ? AND after_review_event_id = ?""",
        (digest, baseline),
    ):
        reviewer = _canonical_hidden_reviewer(actual_reviewer, "verified hidden-key authority")
        segment = _canonical_hidden_segment(actual_segment, "verified hidden-key authority")
        verified[reviewer].add(segment)
    if verified != complete:
        raise RuntimeError("durable hidden-key authority does not exactly cover the retained lifetime set")
    return {
        "policySemanticSha256": digest,
        "hiddenKeysImported": inserted,
        "hiddenKeysDurable": sum(len(keys) for keys in verified.values()),
    }


def inspect(data_dir: Path, db_path: Path) -> dict[str, object]:
    conn = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True, timeout=30)
    try:
        maximum = database_preflight(conn)
    finally:
        conn.close()
    session_path = data_dir / SESSION_FILE
    return {
        "database": str(db_path.resolve()),
        "maxReviewEventId": maximum,
        "sessionPresent": session_path.is_file(),
        "policyPresent": (data_dir / POLICY_FILE).is_file(),
    }


def prepare_maintenance_revocation(data_dir: Path, *, check_runtime: bool = True) -> dict[str, object]:
    """Block auto-resume before schema/FK maintenance; deliberately independent of DB health."""
    resolved = data_dir.resolve()
    with acquire_cortex_lock(resolved):
        if check_runtime:
            require_runtime_offline()
        session_path = resolved / SESSION_FILE
        if not session_path.is_file():
            raise RuntimeError(f"remembered reviewer state is missing: {session_path}")
        marker = resolved / REVOCATION_FILE
        if marker.exists() and not marker.is_file():
            raise RuntimeError(f"revocation authority is not a regular file: {marker}")
        if not marker.exists():
            atomic_write(
                marker,
                (
                    json.dumps(
                        {"reason": "review_pilot_schema_maintenance", "createdUnix": int(time.time())}
                    )
                    + "\n"
                ).encode("utf-8"),
            )
        return {"revocation": str(marker), "session": str(session_path), "autoResumeBlocked": True}


def _activate_locked(
    data_dir: Path,
    db_path: Path,
    *,
    expected_max_review_event_id: int,
    expected_policy_sha256: str | None = None,
    credential_session: Path | None = None,
    replace_roster_before_activity: bool = False,
    focus_additions: tuple[str, ...] = (),
    expected_focus_sha256: str | None = None,
    check_runtime: bool = True,
    fail_after_revocation_for_test: bool = False,
) -> dict[str, object]:
    data_dir = data_dir.resolve()
    db_path = db_path.resolve()
    if check_runtime:
        require_runtime_offline()
    # Public ``activate`` holds Cortex's process lock across this proof and every mutation below.
    # The active policy must never be armed against a merely similar or accidentally narrowed set.
    focus_path = data_dir / VOICE_FOCUS_FILE
    focus_replacement_bytes: bytes | None = None
    if focus_additions:
        original_focus_sha256 = sha256_file(focus_path)
        if not replace_roster_before_activity:
            raise RuntimeError("focus additions are allowed only during a pristine roster replacement")
        if expected_focus_sha256 is None:
            raise RuntimeError("focus additions require --expected-focus-sha256")
        if original_focus_sha256 != expected_focus_sha256.lower():
            raise RuntimeError(
                f"voice-focus CAS mismatch: expected {expected_focus_sha256}, found {original_focus_sha256}"
            )
        if len(focus_additions) != len(set(focus_additions)):
            raise RuntimeError("focus additions contain duplicate segment ids")
        current_ids = load_voice_focus_ids(data_dir)
        overlap = current_ids.intersection(focus_additions)
        if overlap:
            raise RuntimeError(f"focus additions already exist in the active focus: {sorted(overlap)}")
        proposed_ids = current_ids.union(focus_additions)
        expected_contract = load_pilot_focus_contract()
        initial_focus = focus_evidence(proposed_ids)
        if (
            initial_focus.segment_id_count != expected_contract.segment_id_count
            or initial_focus.sorted_unique_segment_ids_sha256
            != expected_contract.sorted_unique_segment_ids_sha256
        ):
            raise RuntimeError("active focus plus the requested additions does not match the tracked contract")
        raw_focus = strict_json_loads(focus_path.read_text(encoding="utf-8"))
        if not isinstance(raw_focus, dict):
            raise RuntimeError(f"{VOICE_FOCUS_FILE} root must be an object")
        raw_focus["segment_ids"] = sorted(proposed_ids)
        focus_replacement_bytes = (json.dumps(raw_focus, ensure_ascii=False, indent=2) + "\n").encode(
            "utf-8"
        )
    else:
        if expected_focus_sha256 is not None:
            raise RuntimeError("--expected-focus-sha256 is valid only with --focus-addition")
        initial_focus = verify_controlled_pilot_focus(data_dir)
        original_focus_sha256 = sha256_file(focus_path)
    session_path = data_dir / SESSION_FILE
    policy_path = data_dir / POLICY_FILE
    marker_path = data_dir / REVOCATION_FILE
    if not session_path.is_file():
        raise RuntimeError(f"remembered reviewer state is missing: {session_path}")
    existing_policy: dict[str, object] | None = None
    if policy_path.is_file():
        if expected_policy_sha256 is None:
            raise RuntimeError("policy already exists; replacement requires --expected-policy-sha256")
        actual = sha256_file(policy_path)
        if actual != expected_policy_sha256.lower():
            raise RuntimeError(f"policy CAS mismatch: expected {expected_policy_sha256}, found {actual}")
        try:
            parsed_existing = strict_json_loads(policy_path.read_text(encoding="utf-8"))
            existing_policy = (
                validate_replaceable_existing_policy(parsed_existing, POLICY_FILE)
                if replace_roster_before_activity
                else validate_pilot_policy(parsed_existing, POLICY_FILE)
            )
        except OSError as error:
            raise RuntimeError(f"existing policy cannot be read: {error}") from error
    elif expected_policy_sha256 is not None:
        raise RuntimeError("policy CAS expected an existing file, but none exists")

    conn = sqlite3.connect(db_path, timeout=30, isolation_level=None)
    marker_written = False
    temp_policy = policy_path.with_name(f".{POLICY_FILE}.prepared.{uuid.uuid4().hex}")
    temp_session = session_path.with_name(f".{SESSION_FILE}.prepared.{uuid.uuid4().hex}")
    temp_focus = focus_path.with_name(f".{VOICE_FOCUS_FILE}.prepared.{uuid.uuid4().hex}")
    try:
        conn.execute("PRAGMA busy_timeout=30000")
        conn.execute("BEGIN IMMEDIATE")
        maximum = database_preflight(conn)
        if maximum != expected_max_review_event_id:
            raise RuntimeError(
                f"review-event CAS mismatch: expected {expected_max_review_event_id}, found {maximum}"
            )
        # A schema-58 pilot may already have real post-baseline work.  Re-activation is a storage
        # bridge, never a fresh budget: retain its exact semantic baseline.  Only the first activation
        # starts at the current maximum.
        if replace_roster_before_activity:
            if existing_policy is None or credential_session is None:
                raise RuntimeError("roster replacement requires an existing policy and --credential-session")
            require_pristine_roster_replacement(conn, existing_policy, maximum)
            live_session = strict_json_loads(session_path.read_text(encoding="utf-8"))
            if not isinstance(live_session, dict):
                raise RuntimeError(f"{SESSION_FILE} root must be an object")
            validate_saved_session_shape(live_session)
            remembered_old = validate_replaceable_existing_policy(
                live_session.get("pilot_policy"), f"{SESSION_FILE} remembered pilot policy"
            )
            if remembered_old != existing_policy:
                raise RuntimeError("current remembered session is not bound to the replaceable policy")
            source_path = credential_session.resolve()
            if not source_path.is_file():
                raise RuntimeError(f"credential session is missing or not a regular file: {source_path}")
            original_session = strict_json_loads(source_path.read_text(encoding="utf-8"))
            policy = pilot_policy(int(existing_policy["after_review_event_id"]))
        else:
            policy = existing_policy if existing_policy is not None else pilot_policy(maximum)
            original_session = strict_json_loads(session_path.read_text(encoding="utf-8"))
        narrowed = narrow_session(original_session, db_path, policy)
        if replace_roster_before_activity:
            # Old cookies and assignments belong to the retired roster generation. Pairing links are
            # retained, but every mutable/leased cache is reset so Rubar and Alle claim a clean batch.
            narrowed["sessions"] = []
            narrowed["spot_checks"] = []
            narrowed["pilot_spot_checks"] = []
        hidden_authority = import_pilot_hidden_authority(conn, policy, narrowed)

        # Authority first. From this point until both replacements verify, every restart is denied.
        atomic_write(
            marker_path,
            (json.dumps({"reason": "review_pilot_activation", "eventId": maximum}) + "\n").encode("utf-8"),
        )
        marker_written = True
        if fail_after_revocation_for_test:
            raise RuntimeError("injected activation failure after durable revocation")

        stamp = f"{int(time.time())}_{uuid.uuid4().hex[:12]}"
        backup = data_dir / "pilot_activation_backups" / stamp
        backup.mkdir(parents=True, exist_ok=False)
        shutil.copy2(session_path, backup / SESSION_FILE)
        if policy_path.is_file():
            shutil.copy2(policy_path, backup / POLICY_FILE)
        else:
            (backup / f"{POLICY_FILE}.ABSENT").write_text("absent before activation\n", encoding="utf-8")
        shutil.copy2(focus_path, backup / VOICE_FOCUS_FILE)
        backup_manifest = {
            "schema": 1,
            "maxReviewEventId": maximum,
            "sessionSha256": sha256_file(backup / SESSION_FILE),
            "policySha256": sha256_file(backup / POLICY_FILE) if (backup / POLICY_FILE).is_file() else None,
            "voiceFocusSha256": sha256_file(backup / VOICE_FOCUS_FILE),
        }
        atomic_write(
            backup / "ACTIVATION_BACKUP.json",
            (json.dumps(backup_manifest, indent=2) + "\n").encode("utf-8"),
        )

        policy_bytes = (json.dumps(policy, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        session_bytes = (json.dumps(narrowed, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        atomic_write(temp_policy, policy_bytes)
        atomic_write(temp_session, session_bytes)
        if focus_replacement_bytes is not None:
            atomic_write(temp_focus, focus_replacement_bytes)
        if database_preflight(conn) != maximum:
            raise RuntimeError("review-event CAS changed while activation held the database reservation")

        # Re-read the live focus at the last safe point before either operating file is promoted.
        # The app/importer cannot race the held Cortex lock; an external/manual edit still fails here.
        if sha256_file(focus_path) != original_focus_sha256:
            raise RuntimeError("controlled-pilot voice focus changed during activation")
        if focus_replacement_bytes is None:
            pre_promotion_focus = verify_controlled_pilot_focus(data_dir)
            if pre_promotion_focus != initial_focus:
                raise RuntimeError("controlled-pilot voice focus changed during activation")

        os.replace(temp_policy, policy_path)
        os.replace(temp_session, session_path)
        if focus_replacement_bytes is not None:
            os.replace(temp_focus, focus_path)
        if policy_path.read_bytes() != policy_bytes or session_path.read_bytes() != session_bytes:
            raise RuntimeError("promoted pilot state failed byte verification")
        reloaded_policy = validate_pilot_policy(
            strict_json_loads(policy_path.read_text(encoding="utf-8")), "promoted pilot policy"
        )
        reloaded = strict_json_loads(session_path.read_text(encoding="utf-8"))
        if not isinstance(reloaded, dict):
            raise RuntimeError("promoted remembered session is not an object")
        validate_saved_session_shape(reloaded)
        remembered_policy = validate_pilot_policy(reloaded.get("pilot_policy"), "remembered pilot policy")
        if reloaded_policy != policy or remembered_policy != policy:
            raise RuntimeError("remembered session policy differs from the promoted operating policy")
        pre_commit_focus = verify_controlled_pilot_focus(data_dir)
        if pre_commit_focus != initial_focus:
            raise RuntimeError("controlled-pilot voice focus changed before activation commit")
        conn.execute("COMMIT")
        marker_path.unlink()  # removed LAST; failure leaves the safely narrowed session paused
        marker_written = False
        return {
            "policy": str(policy_path),
            "policySha256": sha256_file(policy_path),
            "session": str(session_path),
            "sessionSha256": sha256_file(session_path),
            "backup": str(backup),
            "afterReviewEventId": policy["after_review_event_id"],
            "activationMaxReviewEventId": maximum,
            "reviewers": list(REVIEWERS),
            "rosterReplacedBeforeActivity": replace_roster_before_activity,
            "credentialSessionSha256": sha256_file(credential_session.resolve())
            if credential_session is not None
            else None,
            "maxCorpusActions": TOTAL_CAP,
            "maxHiddenQcActions": TOTAL_HIDDEN_QC,
            "maxCompensatedUiActions": MAX_COMPENSATED_UI_ACTIONS,
            "controlledPilotFocusCount": initial_focus.segment_id_count,
            "controlledPilotFocusDigest": initial_focus.sorted_unique_segment_ids_sha256,
            **hidden_authority,
        }
    except Exception:
        try:
            conn.execute("ROLLBACK")
        except sqlite3.Error:
            pass
        if marker_written:
            # Deliberately retained. The operator may repair from the backup, but the old unrestricted
            # session cannot resume after an interrupted activation.
            pass
        raise
    finally:
        conn.close()
        for temp in (temp_policy, temp_session, temp_focus):
            try:
                temp.unlink()
            except FileNotFoundError:
                pass


def activate(
    data_dir: Path,
    db_path: Path,
    *,
    expected_max_review_event_id: int,
    expected_policy_sha256: str | None = None,
    credential_session: Path | None = None,
    replace_roster_before_activity: bool = False,
    focus_additions: tuple[str, ...] = (),
    expected_focus_sha256: str | None = None,
    check_runtime: bool = True,
    fail_after_revocation_for_test: bool = False,
) -> dict[str, object]:
    """Hold Cortex's process authority across offline proof, DB CAS and both promotions."""
    resolved_data_dir = data_dir.resolve()
    with acquire_cortex_lock(resolved_data_dir):
        return _activate_locked(
            resolved_data_dir,
            db_path.resolve(),
            expected_max_review_event_id=expected_max_review_event_id,
            expected_policy_sha256=expected_policy_sha256,
            credential_session=credential_session,
            replace_roster_before_activity=replace_roster_before_activity,
            focus_additions=focus_additions,
            expected_focus_sha256=expected_focus_sha256,
            check_runtime=check_runtime,
            fail_after_revocation_for_test=fail_after_revocation_for_test,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=None)
    parser.add_argument("--db", type=Path, default=None)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--inspect", action="store_true")
    mode.add_argument("--prepare-maintenance-revocation", action="store_true")
    parser.add_argument("--expected-max-review-event-id", type=int)
    parser.add_argument("--expected-policy-sha256")
    parser.add_argument("--replace-roster-before-activity", action="store_true")
    parser.add_argument("--credential-session", type=Path)
    parser.add_argument(
        "--focus-addition",
        action="append",
        default=[],
        help="exact segment id to add atomically during a pristine roster replacement (repeatable)",
    )
    parser.add_argument("--expected-focus-sha256")
    args = parser.parse_args()
    data_dir = (args.data_dir or default_data_dir()).resolve()
    db_path = (args.db or data_dir / "cortex-speech.db").resolve()
    try:
        if args.prepare_maintenance_revocation:
            print(json.dumps(prepare_maintenance_revocation(data_dir), indent=2))
            return 0
        if args.inspect:
            print(json.dumps(inspect(data_dir, db_path), indent=2))
            return 0
        if args.expected_max_review_event_id is None or args.expected_max_review_event_id < 0:
            raise RuntimeError("activation requires --expected-max-review-event-id from a fresh --inspect")
        if args.replace_roster_before_activity != (args.credential_session is not None):
            raise RuntimeError(
                "--replace-roster-before-activity and --credential-session must be supplied together"
            )
        if bool(args.focus_addition) != (args.expected_focus_sha256 is not None):
            raise RuntimeError("--focus-addition and --expected-focus-sha256 must be supplied together")
        result = activate(
            data_dir,
            db_path,
            expected_max_review_event_id=args.expected_max_review_event_id,
            expected_policy_sha256=args.expected_policy_sha256,
            credential_session=args.credential_session,
            replace_roster_before_activity=args.replace_roster_before_activity,
            focus_additions=tuple(args.focus_addition),
            expected_focus_sha256=args.expected_focus_sha256,
        )
        print(json.dumps(result, indent=2))
        return 0
    except Exception as error:
        print(f"REFUSED: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
