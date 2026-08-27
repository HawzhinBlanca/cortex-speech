#!/usr/bin/env python3
"""Safely replace the completed two-person pilot with a single reviewer's full provisional first pass.

The transition is offline and compare-and-swap guarded. It preserves every review/payment row and
that reviewer's existing pairing token, clears stale cookies/leases/checks, stores the exact campaign and
focus contract inside SQLite, retires the pilot policy, and leaves a revocation marker behind on any
interrupted mutation. While this campaign is active, the Rust runtime blocks every dataset/training
export until an independent second pass has been implemented and completed.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import sys
import time
import uuid
from pathlib import Path

from activate_review_pilot import (
    POLICY_FILE,
    REVIEWERS,
    REVOCATION_FILE,
    SESSION_FILE,
    acquire_cortex_lock,
    atomic_write,
    database_preflight,
    default_data_dir,
    require_runtime_offline,
    sha256_file,
    validate_pilot_policy,
)
from check_reviewer_links_live import strict_json_loads, validate_saved_session_shape
from pilot_focus_contract import focus_evidence, load_voice_focus_ids

CAMPAIGN_SETTINGS_KEY = "review_campaign.sequential_first_pass.v1"
CAMPAIGN_MODE = "sequential_first_pass"
CAMPAIGN_STATUS = "first_pass_active"
# The first pass is run by the first pilot reviewer; never a second hardcoded copy of a name.
REVIEWER = REVIEWERS[0]
VOICE_FOCUS_FILE = "voice_focus.json"
CAMPAIGN_NAMESPACE = uuid.UUID("4ad54d71-f76b-4e65-aaed-87d12fc52bd9")


def _canonical_path(path: Path) -> str:
    return os.path.normcase(os.path.abspath(path))


def campaign_policy(*, baseline: int, activation_max: int, focus_count: int, focus_sha256: str) -> dict[str, object]:
    identity = f"{baseline}:{activation_max}:{focus_count}:{focus_sha256}:{REVIEWER.lower()}"
    return {
        "schema_version": 1,
        "campaign_id": str(uuid.uuid5(CAMPAIGN_NAMESPACE, identity)),
        "mode": CAMPAIGN_MODE,
        "status": CAMPAIGN_STATUS,
        "reviewer": REVIEWER,
        "after_review_event_id": baseline,
        "activated_at_review_event_id": activation_max,
        "focus_segment_count": focus_count,
        "focus_sha256": focus_sha256,
        "provisional_export_block": True,
        "independent_second_pass_required": True,
    }


def validate_campaign(value: object) -> dict[str, object]:
    expected_keys = {
        "schema_version", "campaign_id", "mode", "status", "reviewer",
        "after_review_event_id", "activated_at_review_event_id", "focus_segment_count",
        "focus_sha256", "provisional_export_block", "independent_second_pass_required",
    }
    if not isinstance(value, dict) or set(value) != expected_keys:
        raise RuntimeError("sequential campaign fields do not exactly match the runtime contract")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise RuntimeError("sequential campaign schema must be integer version 1")
    try:
        parsed_id = uuid.UUID(str(value["campaign_id"]))
    except (ValueError, AttributeError) as error:
        raise RuntimeError("sequential campaign id must be a canonical UUID") from error
    if str(parsed_id) != value["campaign_id"]:
        raise RuntimeError("sequential campaign id must be a lowercase hyphenated UUID")
    if value["mode"] != CAMPAIGN_MODE or value["status"] != CAMPAIGN_STATUS or value["reviewer"] != REVIEWER:
        raise RuntimeError("sequential campaign mode/status/reviewer is not authorized")
    baseline = value["after_review_event_id"]
    activation = value["activated_at_review_event_id"]
    count = value["focus_segment_count"]
    if any(type(item) is not int for item in (baseline, activation, count)):
        raise RuntimeError("sequential campaign boundaries and focus count must be integers")
    if baseline < 0 or activation < baseline or count <= 0:
        raise RuntimeError("sequential campaign contains invalid boundaries or focus count")
    digest = value["focus_sha256"]
    if not isinstance(digest, str) or len(digest) != 64 or any(ch not in "0123456789abcdef" for ch in digest):
        raise RuntimeError("sequential campaign focus digest must be lowercase SHA-256")
    if value["provisional_export_block"] is not True or value["independent_second_pass_required"] is not True:
        raise RuntimeError("sequential first-pass exports must remain blocked pending independent review")
    return value


def _load_json_object(path: Path, label: str) -> dict[str, object]:
    try:
        value = strict_json_loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RuntimeError(f"{label} cannot be read: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"{label} root must be an object")
    return value


def _narrow_session(session: dict[str, object], db_path: Path) -> dict[str, object]:
    validate_saved_session_shape(session)
    if _canonical_path(Path(str(session["db_path"]))) != _canonical_path(db_path):
        raise RuntimeError(f"{SESSION_FILE} belongs to a different database")
    pairing = session["reviewers"]
    assert isinstance(pairing, dict)
    kept = {token: name for token, name in pairing.items() if name.strip().lower() == REVIEWER.lower()}
    if len(kept) != 1:
        raise RuntimeError(f"{SESSION_FILE} must contain exactly one durable pairing token for {REVIEWER}")
    narrowed = dict(session)
    narrowed["reviewers"] = kept
    narrowed["sessions"] = []
    narrowed["spot_checks"] = []
    narrowed["pilot_spot_checks"] = []
    narrowed["pilot_policy"] = None
    return narrowed


def _verify_post_baseline(conn: sqlite3.Connection, baseline: int, activation_max: int) -> dict[str, int]:
    rows = conn.execute(
        "SELECT id, reviewer, action, source FROM review_events WHERE id > ? ORDER BY id", (baseline,)
    ).fetchall()
    if len(rows) != activation_max - baseline:
        raise RuntimeError("post-pilot review history is not a contiguous immutable event sequence")
    corpus = hidden = 0
    for event_id, reviewer, action, source in rows:
        if reviewer.strip().lower() != REVIEWER.lower():
            raise RuntimeError(f"event {event_id} belongs to {reviewer!r}; single-reviewer takeover is refused")
        if action not in {"accept", "edit", "reject", "skip"}:
            raise RuntimeError(f"event {event_id} contains unsupported action {action!r}")
        if source == "couch":
            corpus += 1
        elif source == "couch_spot_check":
            hidden += 1
        else:
            raise RuntimeError(f"event {event_id} has unexpected paid-review source {source!r}")
    return {"retainedCorpusEvents": corpus, "retainedInvalidPilotChecks": hidden}


def _database_backup(conn: sqlite3.Connection, destination: Path) -> None:
    backup = sqlite3.connect(destination)
    try:
        conn.backup(backup)
        quick = [str(row[0]) for row in backup.execute("PRAGMA quick_check")]
        if quick != ["ok"]:
            raise RuntimeError(f"activation database backup failed integrity verification: {quick!r}")
    finally:
        backup.close()


def inspect(data_dir: Path, db_path: Path) -> dict[str, object]:
    focus = focus_evidence(load_voice_focus_ids(data_dir))
    conn = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True, timeout=30)
    try:
        maximum = database_preflight(conn)
        existing = conn.execute("SELECT value FROM settings WHERE key = ?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()
        policy = validate_campaign(strict_json_loads(existing[0])) if existing else None
    finally:
        conn.close()
    return {
        "database": str(db_path.resolve()),
        "maxReviewEventId": maximum,
        "focusSegmentCount": focus.segment_id_count,
        "focusSha256": focus.sorted_unique_segment_ids_sha256,
        "pilotPolicyPresent": (data_dir / POLICY_FILE).is_file(),
        "campaign": policy,
        "revoked": (data_dir / REVOCATION_FILE).is_file(),
    }


def activate(
    data_dir: Path,
    db_path: Path,
    *,
    expected_max_review_event_id: int,
    expected_pilot_policy_sha256: str,
    check_runtime: bool = True,
) -> dict[str, object]:
    data_dir = data_dir.resolve()
    db_path = db_path.resolve()
    with acquire_cortex_lock(data_dir):
        if check_runtime:
            require_runtime_offline()
        session_path = data_dir / SESSION_FILE
        pilot_path = data_dir / POLICY_FILE
        focus_path = data_dir / VOICE_FOCUS_FILE
        marker_path = data_dir / REVOCATION_FILE
        for path in (db_path, session_path, focus_path):
            if not path.is_file():
                raise RuntimeError(f"required activation input is missing: {path}")
        if pilot_path.is_file() and sha256_file(pilot_path) != expected_pilot_policy_sha256.lower():
            raise RuntimeError("pilot policy CAS mismatch; activation is refused")

        focus = focus_evidence(load_voice_focus_ids(data_dir))
        session = _load_json_object(session_path, SESSION_FILE)
        narrowed = _narrow_session(session, db_path)
        conn = sqlite3.connect(db_path, timeout=30, isolation_level=None)
        marker_written = marker_path.is_file()
        try:
            conn.execute("PRAGMA busy_timeout=30000")
            maximum = database_preflight(conn)
            if maximum != expected_max_review_event_id:
                raise RuntimeError(
                    f"review-event CAS mismatch: expected {expected_max_review_event_id}, found {maximum}"
                )

            existing = conn.execute("SELECT value FROM settings WHERE key = ?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()
            if pilot_path.is_file():
                pilot = validate_pilot_policy(_load_json_object(pilot_path, POLICY_FILE), POLICY_FILE)
                baseline = int(pilot["after_review_event_id"])
                remembered_raw = session.get("pilot_policy")
                if remembered_raw is not None:
                    remembered = validate_pilot_policy(remembered_raw, f"{SESSION_FILE} remembered pilot policy")
                    if pilot != remembered:
                        raise RuntimeError("remembered session is not bound to the live pilot policy")
                elif not (existing and marker_written):
                    raise RuntimeError("live pilot policy has no matching remembered policy")
            elif existing:
                baseline = int(validate_campaign(strict_json_loads(existing[0]))["after_review_event_id"])
            else:
                raise RuntimeError("neither the pilot nor an interrupted sequential campaign exists")

            intended = validate_campaign(campaign_policy(
                baseline=baseline,
                activation_max=maximum,
                focus_count=focus.segment_id_count,
                focus_sha256=focus.sorted_unique_segment_ids_sha256,
            ))
            history = _verify_post_baseline(conn, baseline, maximum)
            if existing and validate_campaign(strict_json_loads(existing[0])) != intended:
                raise RuntimeError("existing sequential campaign differs from the exact intended contract")
            if existing and not pilot_path.exists() and not marker_written:
                if session != narrowed:
                    raise RuntimeError("sequential campaign is active but the remembered session is not safely narrowed")
                return {
                    "campaign": intended,
                    "reviewers": [REVIEWER],
                    "sessionSha256": sha256_file(session_path),
                    "backup": None,
                    "pilotRetired": True,
                    "alreadyActive": True,
                    "exportsBlockedPendingIndependentSecondPass": True,
                    **history,
                }

            if not marker_written:
                stamp = f"{int(time.time())}_{intended['campaign_id']}"
                backup_dir = data_dir / "sequential_activation_backups" / stamp
                backup_dir.mkdir(parents=True, exist_ok=False)
                _database_backup(conn, backup_dir / "cortex-speech.db")
                shutil.copy2(session_path, backup_dir / SESSION_FILE)
                shutil.copy2(focus_path, backup_dir / VOICE_FOCUS_FILE)
                if pilot_path.is_file():
                    shutil.copy2(pilot_path, backup_dir / POLICY_FILE)
                manifest = {
                    "schema": 1,
                    "databaseSha256": sha256_file(backup_dir / "cortex-speech.db"),
                    "sessionSha256": sha256_file(backup_dir / SESSION_FILE),
                    "focusSha256": sha256_file(backup_dir / VOICE_FOCUS_FILE),
                    "pilotPolicySha256": sha256_file(backup_dir / POLICY_FILE)
                    if (backup_dir / POLICY_FILE).is_file()
                    else None,
                    "campaign": intended,
                }
                atomic_write(
                    backup_dir / "ACTIVATION_BACKUP.json",
                    (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
                )
                atomic_write(
                    marker_path,
                    (json.dumps({"reason": "sequential_review_activation", "backup": str(backup_dir),
                                 "campaignId": intended["campaign_id"]}) + "\n").encode("utf-8"),
                )
                marker_written = True
            else:
                marker = _load_json_object(marker_path, REVOCATION_FILE)
                if marker.get("campaignId") != intended["campaign_id"]:
                    raise RuntimeError("revocation marker belongs to a different interrupted operation")
                backup_dir = Path(str(marker.get("backup", "")))
                if not (backup_dir / "ACTIVATION_BACKUP.json").is_file():
                    raise RuntimeError("interrupted activation has no verified recovery backup")

            policy_json = json.dumps(intended, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
            conn.execute("BEGIN IMMEDIATE")
            if database_preflight(conn) != maximum:
                raise RuntimeError("review history changed while activation held the database reservation")
            current = conn.execute("SELECT value FROM settings WHERE key = ?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()
            if current is None:
                conn.execute("INSERT INTO settings(key, value) VALUES(?, ?)", (CAMPAIGN_SETTINGS_KEY, policy_json))
            elif validate_campaign(strict_json_loads(current[0])) != intended:
                raise RuntimeError("sequential campaign compare-and-swap failed")
            conn.execute("COMMIT")

            atomic_write(session_path, (json.dumps(narrowed, ensure_ascii=False, indent=2) + "\n").encode("utf-8"))
            if pilot_path.is_file():
                pilot_path.unlink()

            reloaded = _load_json_object(session_path, SESSION_FILE)
            validate_saved_session_shape(reloaded)
            if reloaded != narrowed or pilot_path.exists():
                raise RuntimeError("promoted single-reviewer session failed verification")
            stored = conn.execute("SELECT value FROM settings WHERE key = ?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()
            if stored is None or validate_campaign(strict_json_loads(stored[0])) != intended:
                raise RuntimeError("promoted sequential campaign failed verification")
            if database_preflight(conn) != maximum:
                raise RuntimeError("durable review history changed during activation")
            marker_path.unlink()
            marker_written = False
            return {
                "campaign": intended,
                "reviewers": [REVIEWER],
                "sessionSha256": sha256_file(session_path),
                "backup": str(backup_dir),
                "pilotRetired": True,
                "exportsBlockedPendingIndependentSecondPass": True,
                **history,
            }
        except Exception:
            try:
                conn.execute("ROLLBACK")
            except sqlite3.Error:
                pass
            # Once any mutation begins, the marker deliberately remains. The app refuses auto-resume.
            raise
        finally:
            conn.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--db", type=Path)
    parser.add_argument("--inspect", action="store_true")
    parser.add_argument("--expected-max-review-event-id", type=int)
    parser.add_argument("--expected-pilot-policy-sha256")
    args = parser.parse_args()
    data_dir = (args.data_dir or default_data_dir()).resolve()
    db_path = (args.db or data_dir / "cortex-speech.db").resolve()
    try:
        if args.inspect:
            print(json.dumps(inspect(data_dir, db_path), indent=2))
            return 0
        if args.expected_max_review_event_id is None or not args.expected_pilot_policy_sha256:
            parser.error("activation requires --expected-max-review-event-id and --expected-pilot-policy-sha256")
        result = activate(
            data_dir,
            db_path,
            expected_max_review_event_id=args.expected_max_review_event_id,
            expected_pilot_policy_sha256=args.expected_pilot_policy_sha256,
        )
        print(json.dumps(result, indent=2))
        return 0
    except (OSError, RuntimeError, sqlite3.Error, ValueError) as error:
        print(f"BLOCKED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
