#!/usr/bin/env python3
"""Atomically narrow an active sequential campaign to one completed named-voice import.

This is an offline, compare-and-swap operation. It never transcribes, touches a model service, or
rewrites review/payment history. The exact completed import becomes the focus, its segment rows get
the owner's speaker name, and the database campaign contract changes in the same revoked maintenance
window. Any interrupted transition leaves Couch auto-resume blocked and can be safely rerun.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

from activate_review_pilot import (
    REVOCATION_FILE,
    SESSION_FILE,
    acquire_cortex_lock,
    atomic_write,
    database_preflight,
    default_data_dir,
    require_runtime_offline,
    sha256_file,
)
from activate_sequential_review import (
    CAMPAIGN_SETTINGS_KEY,
    REVIEWER,
    _database_backup,
    campaign_policy,
    validate_campaign,
)
from check_review_serving_provenance import current_champion_model_ids
from check_reviewer_links_live import strict_json_loads, validate_saved_session_shape
from pilot_focus_contract import VOICE_FOCUS_FILE, focus_evidence, load_voice_focus_ids

MARKER_REASON = "named_voice_focus_activation"


def _load_json_object(path: Path, label: str) -> dict[str, object]:
    try:
        value = strict_json_loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RuntimeError(f"{label} cannot be read: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"{label} root must be an object")
    return value


def _review_history_digest(conn: sqlite3.Connection) -> str:
    digest = hashlib.sha256()
    for row in conn.execute("SELECT * FROM review_events ORDER BY id"):
        digest.update(repr(tuple(row)).encode("utf-8"))
        digest.update(b"\n")
    return digest.hexdigest()


def _active_campaign(conn: sqlite3.Connection) -> dict[str, object]:
    row = conn.execute("SELECT value FROM settings WHERE key = ?", (CAMPAIGN_SETTINGS_KEY,)).fetchone()
    if row is None:
        raise RuntimeError("no active sequential first-pass campaign exists")
    return validate_campaign(strict_json_loads(str(row[0])))


def _validate_speaker_name(value: str) -> str:
    name = value.strip()
    if not name or len(name) > 64 or any(char in "\r\n\0" or ord(char) < 32 for char in name):
        raise RuntimeError("speaker name must be 1-64 printable characters")
    return name


def _canonical_path(path: Path) -> str:
    return os.path.normcase(os.path.abspath(path))


def _count_target_review_events(conn: sqlite3.Connection, segment_ids: set[str]) -> int:
    total = 0
    ordered = sorted(segment_ids)
    for offset in range(0, len(ordered), 800):
        chunk = ordered[offset : offset + 800]
        placeholders = ",".join("?" for _ in chunk)
        total += int(
            conn.execute(
                f"SELECT COUNT(*) FROM review_events WHERE segment_id IN ({placeholders})",
                chunk,
            ).fetchone()[0]
        )
    return total


def _load_import_segments(
    conn: sqlite3.Connection,
    job_id: str,
    expected_source_dir: Path | None,
) -> tuple[dict[str, object], list[sqlite3.Row]]:
    job = conn.execute("SELECT id, dir, total_files, status FROM import_jobs WHERE id = ?", (job_id,)).fetchone()
    if job is None:
        raise RuntimeError(f"import job {job_id} does not exist")
    job_doc = {"id": str(job[0]), "dir": str(job[1]), "total_files": int(job[2]), "status": str(job[3])}
    if job_doc["status"] != "completed" or job_doc["total_files"] <= 0:
        raise RuntimeError("named-voice import must be completed and non-empty")
    source_dir = Path(str(job_doc["dir"]))
    if expected_source_dir is not None and os.path.normcase(os.path.abspath(source_dir)) != os.path.normcase(
        os.path.abspath(expected_source_dir)
    ):
        raise RuntimeError(f"import source CAS mismatch: expected {expected_source_dir}, found {source_dir}")

    prior_factory = conn.row_factory
    conn.row_factory = sqlite3.Row
    try:
        rows = list(
            conn.execute(
                """SELECT f.path, s.id, s.audio_path, s.raw_transcript, s.model_version_id,
                          s.speaker_id, s.review_revision, s.verified
                     FROM import_job_files f
                     LEFT JOIN speech_segments s ON s.audio_path = f.path
                    WHERE f.job_id = ? ORDER BY f.path, s.id""",
                (job_id,),
            )
        )
    finally:
        conn.row_factory = prior_factory
    paths = [str(row["path"]) for row in rows]
    segment_ids = [str(row["id"] or "") for row in rows]
    if len(rows) != job_doc["total_files"] or len(set(paths)) != len(paths):
        raise RuntimeError("import journal is not an exact unique completed source set")
    if any(not segment_id for segment_id in segment_ids) or len(set(segment_ids)) != len(segment_ids):
        raise RuntimeError("import paths do not map one-to-one to unique speech segments")
    missing_audio = [path for path in paths if not Path(path).is_file()]
    if missing_audio:
        raise RuntimeError(f"named-voice import has {len(missing_audio)} missing WAV(s); first={missing_audio[:3]}")
    # The registry is the champion, not `champion.json` — that file is only the startup mirror the app
    # rewrites on every launch, so during the documented register-first/restart-second window it names
    # a champion `model_versions` no longer does. Resolving it here on the same connection fails closed
    # on any ambiguous registry state instead of certifying drafts against a stale pointer.
    champions = current_champion_model_ids(conn)
    bad_drafts = [
        str(row["id"])
        for row in rows
        if not str(row["raw_transcript"] or "").strip()
        or (str(row["raw_transcript"]).startswith("[") and str(row["raw_transcript"]).endswith("]"))
    ]
    wrong_models = [str(row["id"]) for row in rows if str(row["model_version_id"] or "") not in champions]
    if bad_drafts:
        raise RuntimeError(f"named-voice import has {len(bad_drafts)} blank/placeholder draft(s)")
    if wrong_models:
        raise RuntimeError(f"named-voice import has {len(wrong_models)} non-champion draft(s)")
    verified = [str(row["id"]) for row in rows if bool(row["verified"])]
    if verified:
        raise RuntimeError(f"named-voice import is already partially verified ({len(verified)} segment(s))")
    return job_doc, rows


def activate(
    data_dir: Path,
    db_path: Path,
    *,
    speaker_name: str,
    import_job_id: str,
    expected_current_campaign_id: str,
    expected_max_review_event_id: int,
    expected_source_dir: Path | None = None,
    check_runtime: bool = True,
    fail_after_db_commit_for_test: bool = False,
) -> dict[str, object]:
    data_dir = data_dir.resolve()
    db_path = db_path.resolve()
    speaker = _validate_speaker_name(speaker_name)
    with acquire_cortex_lock(data_dir):
        if check_runtime:
            require_runtime_offline()
        session_path = data_dir / SESSION_FILE
        focus_path = data_dir / VOICE_FOCUS_FILE
        marker_path = data_dir / REVOCATION_FILE
        for path in (db_path, session_path, focus_path):
            if not path.is_file():
                raise RuntimeError(f"required named-voice input is missing: {path}")
        session = _load_json_object(session_path, SESSION_FILE)
        validate_saved_session_shape(session)
        if _canonical_path(Path(str(session["db_path"]))) != _canonical_path(db_path):
            raise RuntimeError(f"{SESSION_FILE} belongs to a different database")
        if sorted({str(name).strip().lower() for name in session["reviewers"].values()}) != [REVIEWER.lower()]:
            raise RuntimeError(f"named-voice activation requires the live durable roster to contain only {REVIEWER}")
        if any(
            str(entry.get("reviewer", "")).strip().lower() != REVIEWER.lower()
            for entry in session.get("sessions", [])
        ):
            raise RuntimeError(f"named-voice activation found a saved cookie session that is not {REVIEWER}'s")
        if session.get("spot_checks", []) or session.get("pilot_spot_checks", []) or session.get("pilot_policy") is not None:
            raise RuntimeError("named-voice activation requires the retired pilot state")

        conn = sqlite3.connect(db_path, timeout=30, isolation_level=None)
        try:
            conn.execute("PRAGMA busy_timeout=30000")
            maximum = database_preflight(conn)
            if maximum != expected_max_review_event_id:
                raise RuntimeError(
                    f"review-event CAS mismatch: expected {expected_max_review_event_id}, found {maximum}"
                )
            before_history = _review_history_digest(conn)
            current = _active_campaign(conn)
            job, rows = _load_import_segments(conn, import_job_id, expected_source_dir)
            ids = {str(row["id"]) for row in rows}
            reviewed = _count_target_review_events(conn, ids)
            if reviewed:
                raise RuntimeError(f"named-voice import already has {reviewed} review event(s); fresh full review required")
            current_focus = load_voice_focus_ids(data_dir)
            if not ids.issubset(current_focus):
                raise RuntimeError("named-voice import is not a subset of the currently authorized focus")
            target_focus = focus_evidence(ids)
            intended = validate_campaign(
                campaign_policy(
                    baseline=int(current["after_review_event_id"]),
                    activation_max=maximum,
                    focus_count=target_focus.segment_id_count,
                    focus_sha256=target_focus.sorted_unique_segment_ids_sha256,
                )
            )

            marker: dict[str, object] | None = None
            if marker_path.exists():
                marker = _load_json_object(marker_path, REVOCATION_FILE)
                if (
                    marker.get("reason") != MARKER_REASON
                    or marker.get("fromCampaignId") != expected_current_campaign_id
                    or marker.get("toCampaignId") != intended["campaign_id"]
                ):
                    raise RuntimeError("revocation marker belongs to a different interrupted operation")
                backup_dir = Path(str(marker.get("backup", "")))
                if not (backup_dir / "ACTIVATION_BACKUP.json").is_file():
                    raise RuntimeError("interrupted named-voice activation has no recovery backup")
                if current["campaign_id"] not in {expected_current_campaign_id, intended["campaign_id"]}:
                    raise RuntimeError("interrupted activation campaign state is inconsistent")
            else:
                if current["campaign_id"] != expected_current_campaign_id:
                    raise RuntimeError(
                        f"campaign CAS mismatch: expected {expected_current_campaign_id}, found {current['campaign_id']}"
                    )
                stamp = f"{int(time.time())}_{intended['campaign_id']}"
                backup_dir = data_dir / "named_voice_activation_backups" / stamp
                backup_dir.mkdir(parents=True, exist_ok=False)
                _database_backup(conn, backup_dir / "cortex-speech.db")
                shutil.copy2(session_path, backup_dir / SESSION_FILE)
                shutil.copy2(focus_path, backup_dir / VOICE_FOCUS_FILE)
                manifest = {
                    "schema": 1,
                    "fromCampaign": current,
                    "toCampaign": intended,
                    "speaker": speaker,
                    "importJob": job,
                    "reviewHistorySha256": before_history,
                    "databaseSha256": sha256_file(backup_dir / "cortex-speech.db"),
                    "sessionSha256": sha256_file(backup_dir / SESSION_FILE),
                    "focusSha256": sha256_file(backup_dir / VOICE_FOCUS_FILE),
                }
                atomic_write(
                    backup_dir / "ACTIVATION_BACKUP.json",
                    (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
                )
                atomic_write(
                    marker_path,
                    (
                        json.dumps(
                            {
                                "reason": MARKER_REASON,
                                "backup": str(backup_dir),
                                "fromCampaignId": current["campaign_id"],
                                "toCampaignId": intended["campaign_id"],
                            }
                        )
                        + "\n"
                    ).encode("utf-8"),
                )

            policy_json = json.dumps(intended, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
            conn.execute("BEGIN IMMEDIATE")
            if database_preflight(conn) != maximum or _review_history_digest(conn) != before_history:
                raise RuntimeError("review history changed while named-voice activation held the database")
            live_campaign = _active_campaign(conn)
            if live_campaign["campaign_id"] not in {expected_current_campaign_id, intended["campaign_id"]}:
                raise RuntimeError("campaign changed during named-voice activation")
            renamed = 0
            sorted_ids = sorted(ids)
            for offset in range(0, len(sorted_ids), 800):
                chunk = sorted_ids[offset : offset + 800]
                placeholders = ",".join("?" for _ in chunk)
                renamed += conn.execute(
                    f"""UPDATE speech_segments
                           SET speaker_id = ?,
                               review_revision = COALESCE(review_revision, 0)
                                   + CASE WHEN COALESCE(speaker_id, '') = ? THEN 0 ELSE 1 END
                         WHERE id IN ({placeholders})""",
                    [speaker, speaker, *chunk],
                ).rowcount
            if renamed != len(ids):
                raise RuntimeError(f"speaker naming touched {renamed} rows, expected {len(ids)}")
            conn.execute(
                "UPDATE settings SET value = ? WHERE key = ?",
                (policy_json, CAMPAIGN_SETTINGS_KEY),
            )
            if conn.execute("SELECT changes()").fetchone()[0] != 1:
                raise RuntimeError("active campaign row was not updated exactly once")
            conn.execute("COMMIT")
            if fail_after_db_commit_for_test:
                raise RuntimeError("injected failure after named-voice database commit")

            focus_document = _load_json_object(focus_path, VOICE_FOCUS_FILE)
            focus_document.update(
                {
                    "name": speaker,
                    "activated_at": datetime.now(UTC).isoformat(),
                    "basis": f"completed import job {import_job_id}; exact one-to-one named-voice source",
                    "segment_ids": sorted_ids,
                }
            )
            atomic_write(
                focus_path,
                (json.dumps(focus_document, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
            )

            if load_voice_focus_ids(data_dir) != ids:
                raise RuntimeError("promoted named-voice focus failed exact verification")
            if _active_campaign(conn) != intended:
                raise RuntimeError("promoted named-voice campaign failed verification")
            named = 0
            for offset in range(0, len(sorted_ids), 800):
                chunk = sorted_ids[offset : offset + 800]
                placeholders = ",".join("?" for _ in chunk)
                named += int(
                    conn.execute(
                        f"SELECT COUNT(*) FROM speech_segments WHERE speaker_id = ? AND id IN ({placeholders})",
                        [speaker, *chunk],
                    ).fetchone()[0]
                )
            if named != len(ids) or database_preflight(conn) != maximum or _review_history_digest(conn) != before_history:
                raise RuntimeError("named-voice post-activation proof failed")
            marker_path.unlink()
            return {
                "speaker": speaker,
                "importJobId": import_job_id,
                "sourceDirectory": job["dir"],
                "clips": len(ids),
                "focusSha256": target_focus.sorted_unique_segment_ids_sha256,
                "campaign": intended,
                "reviewHistorySha256": before_history,
                "maxReviewEventId": maximum,
                "sessionSha256": sha256_file(session_path),
                "backup": str(backup_dir),
                "transcriptionRun": False,
                "gpuTouched": False,
            }
        except Exception:
            try:
                conn.execute("ROLLBACK")
            except sqlite3.Error:
                pass
            raise
        finally:
            conn.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--db", type=Path)
    parser.add_argument("--speaker-name", required=True)
    parser.add_argument("--import-job-id", required=True)
    parser.add_argument("--expected-current-campaign-id", required=True)
    parser.add_argument("--expected-max-review-event-id", type=int, required=True)
    parser.add_argument("--expected-source-dir", type=Path)
    args = parser.parse_args()
    data_dir = (args.data_dir or default_data_dir()).resolve()
    db_path = (args.db or data_dir / "cortex-speech.db").resolve()
    try:
        result = activate(
            data_dir,
            db_path,
            speaker_name=args.speaker_name,
            import_job_id=args.import_job_id,
            expected_current_campaign_id=args.expected_current_campaign_id,
            expected_max_review_event_id=args.expected_max_review_event_id,
            expected_source_dir=args.expected_source_dir,
        )
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (OSError, RuntimeError, sqlite3.Error, ValueError) as error:
        print(f"BLOCKED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
