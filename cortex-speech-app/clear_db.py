"""Clear working tables in a harness-minted disposable Cortex profile.

This is intentionally incapable of clearing an arbitrary ``CORTEX_APP_DATA_DIR``. A destructive
run must present two matching capabilities created by ``e2e_profile.cjs``:

* an fsynced sentinel in a fresh, canonical ``cortex-e2e-*`` child of the OS temp directory; and
* a random ``PRAGMA application_id`` marker in a brand-new SQLite database, derived from the run
  token. Unlike a marker table, this keeps the file schema-pristine so the app can bootstrap it.

Every refusal happens before a writable SQLite connection or backup is opened, so the database and
its WAL remain byte-identical. A successful clear uses SQLite's online-backup API while a reserved
writer lock excludes concurrent changes; copying only the main ``.db`` file is not WAL-safe.
"""

from __future__ import annotations

import argparse
import hmac
import json
import os
import re
import sqlite3
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any


TABLES = [
    "speech_segments",
    "segment_hypotheses",
    "segment_edits",
    "dataset_runs",
    "agent_stage_events",
    "agent_import_reports",
]

DB_NAME = "cortex-speech.db"
DISPOSABLE_PREFIX = "cortex-e2e-"
SENTINEL_NAME = ".cortex-e2e-disposable.json"
DISPOSABLE_PURPOSE = "cortex-e2e-disposable-profile"
TOKEN_RE = re.compile(r"^[0-9a-f]{64}$")
HARNESS_RE = re.compile(r"^[A-Za-z0-9_.-]{1,80}$")


class SafetyRefusal(RuntimeError):
    """The caller failed the destructive-operation containment contract."""


def _normalized(path: str | Path) -> str:
    return os.path.normcase(os.path.normpath(os.path.abspath(os.fspath(path))))


def _is_same_path(left: str | Path, right: str | Path) -> bool:
    return _normalized(left) == _normalized(right)


def _application_id_for_token(token: str) -> int:
    # SQLite application_id is a signed 32-bit header field. Derive a non-zero positive marker from
    # the capability token so a copied production DB (normally application_id=0) cannot qualify.
    return (int(token[:8], 16) & 0x7FFFFFFF) or 1


def _read_sentinel(profile: Path) -> dict[str, Any]:
    sentinel_path = profile / SENTINEL_NAME
    try:
        sentinel_stat = sentinel_path.lstat()
    except FileNotFoundError as exc:
        raise SafetyRefusal(f"missing harness-minted sentinel: {sentinel_path}") from exc
    if stat.S_ISLNK(sentinel_stat.st_mode) or not stat.S_ISREG(sentinel_stat.st_mode):
        raise SafetyRefusal("the disposable-profile sentinel must be a regular, non-link file")
    if sentinel_stat.st_nlink != 1:
        raise SafetyRefusal("the disposable-profile sentinel must not be hard-linked")
    if sentinel_stat.st_size <= 0 or sentinel_stat.st_size > 8192:
        raise SafetyRefusal("the disposable-profile sentinel has an invalid size")
    try:
        if sentinel_path.resolve(strict=True).parent != profile:
            raise SafetyRefusal("the disposable-profile sentinel resolves outside its profile")
        with sentinel_path.open("r", encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise SafetyRefusal(f"could not read the disposable-profile sentinel: {exc}") from exc
    if not isinstance(value, dict):
        raise SafetyRefusal("the disposable-profile sentinel is not a JSON object")
    return value


def validate_disposable_profile() -> tuple[Path, str, str, dict[str, Any]]:
    raw_profile = os.environ.get("CORTEX_APP_DATA_DIR")
    if not raw_profile:
        raise SafetyRefusal("CORTEX_APP_DATA_DIR is required; the production default is never eligible")

    lexical_profile = Path(os.path.abspath(raw_profile))
    try:
        profile = lexical_profile.resolve(strict=True)
        temp_root = Path(tempfile.gettempdir()).resolve(strict=True)
    except OSError as exc:
        raise SafetyRefusal(f"could not resolve the disposable profile and temp root: {exc}") from exc

    if not profile.is_dir():
        raise SafetyRefusal("CORTEX_APP_DATA_DIR is not a directory")
    # A junction/symlink may lexically sit below TEMP while resolving to a relocated production DB.
    if not _is_same_path(lexical_profile, profile):
        raise SafetyRefusal("CORTEX_APP_DATA_DIR is a symlink, junction, or other path alias")
    if profile.parent != temp_root or not profile.name.startswith(DISPOSABLE_PREFIX):
        raise SafetyRefusal(
            "CORTEX_APP_DATA_DIR is not a direct harness-minted cortex-e2e-* child of the temp root"
        )

    token = os.environ.get("CORTEX_TEST_PROFILE_TOKEN", "")
    harness = os.environ.get("CORTEX_TEST_PROFILE_HARNESS", "")
    if not TOKEN_RE.fullmatch(token):
        raise SafetyRefusal("CORTEX_TEST_PROFILE_TOKEN is missing or malformed")
    if not HARNESS_RE.fullmatch(harness):
        raise SafetyRefusal("CORTEX_TEST_PROFILE_HARNESS is missing or malformed")

    sentinel = _read_sentinel(profile)
    expected_scalars = {
        "schema": 1,
        "purpose": DISPOSABLE_PURPOSE,
        "harness": harness,
    }
    for key, expected in expected_scalars.items():
        if sentinel.get(key) != expected:
            raise SafetyRefusal(f"disposable-profile sentinel {key!r} does not match this run")
    sentinel_token = sentinel.get("profileToken")
    if not isinstance(sentinel_token, str) or not hmac.compare_digest(sentinel_token, token):
        raise SafetyRefusal("disposable-profile sentinel token does not match this run")
    if sentinel.get("sqliteApplicationId") != _application_id_for_token(token):
        raise SafetyRefusal("disposable-profile sentinel SQLite marker does not match this run token")
    canonical_profile = sentinel.get("canonicalProfile")
    if not isinstance(canonical_profile, str) or not _is_same_path(canonical_profile, profile):
        raise SafetyRefusal("disposable-profile sentinel is bound to a different canonical path")
    if not isinstance(sentinel.get("createdAtUtc"), str) or not sentinel["createdAtUtc"].strip():
        raise SafetyRefusal("disposable-profile sentinel has no creation timestamp")
    return profile, token, harness, sentinel


def _validate_database_file(profile: Path) -> tuple[Path, os.stat_result]:
    db_path = profile / DB_NAME
    try:
        db_lstat = db_path.lstat()
    except FileNotFoundError as exc:
        raise SafetyRefusal("the disposable SQLite database and its test marker do not exist") from exc
    if stat.S_ISLNK(db_lstat.st_mode) or not stat.S_ISREG(db_lstat.st_mode):
        raise SafetyRefusal("the disposable SQLite database must be a regular, non-link file")
    if db_lstat.st_nlink != 1:
        raise SafetyRefusal("the disposable SQLite database must not be hard-linked")
    try:
        if db_path.resolve(strict=True).parent != profile:
            raise SafetyRefusal("the disposable SQLite database resolves outside its profile")
    except OSError as exc:
        raise SafetyRefusal(f"could not resolve the disposable SQLite database: {exc}") from exc
    return db_path, db_lstat


def _verify_marker_on_connection(
    conn: sqlite3.Connection,
    token: str,
    harness: str,
    sentinel: dict[str, Any],
) -> None:
    try:
        row = conn.execute("PRAGMA application_id").fetchone()
    except sqlite3.Error as exc:
        raise SafetyRefusal("SQLite test-profile marker is missing or unreadable") from exc
    expected = _application_id_for_token(token)
    if sentinel.get("sqliteApplicationId") != expected or row != (expected,):
        raise SafetyRefusal("SQLite test-profile marker does not match the harness sentinel")


def _verify_marker_without_writes(
    db_path: Path,
    token: str,
    harness: str,
    sentinel: dict[str, Any],
) -> None:
    # immutable=1 is deliberate: a normal read-only WAL connection may create/update -shm. The
    # marker is checkpointed into the main DB by initialization, so this validation can ignore WAL
    # and preserve both DB and WAL bytes exactly when it refuses.
    uri = db_path.resolve(strict=True).as_uri() + "?mode=ro&immutable=1"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=5)
    except sqlite3.Error as exc:
        raise SafetyRefusal(f"could not open the SQLite marker without writes: {exc}") from exc
    try:
        _verify_marker_on_connection(conn, token, harness, sentinel)
    finally:
        conn.close()


def initialize_test_profile(profile: Path, token: str, harness: str, sentinel: dict[str, Any]) -> None:
    unexpected = [entry.name for entry in profile.iterdir() if entry.name != SENTINEL_NAME]
    if unexpected:
        raise SafetyRefusal(
            "test-profile initialization requires a brand-new directory containing only its sentinel"
        )

    db_path = profile / DB_NAME
    flags = os.O_CREAT | os.O_EXCL | os.O_RDWR
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    try:
        fd = os.open(db_path, flags, 0o600)
    except FileExistsError as exc:
        raise SafetyRefusal("refusing to attach a test marker to an existing SQLite database") from exc
    os.close(fd)

    try:
        conn = sqlite3.connect(db_path, timeout=30)
        try:
            conn.execute("PRAGMA journal_mode=DELETE")
            conn.execute("PRAGMA synchronous=FULL")
            conn.execute(f"PRAGMA application_id={_application_id_for_token(token)}")
            conn.commit()
        finally:
            conn.close()
        # Windows' FlushFileBuffers requires a handle opened for writing; fsync on an ``rb`` handle
        # raises EBADF even though the SQLite commit itself succeeded.
        with db_path.open("r+b") as handle:
            os.fsync(handle.fileno())
        _verify_marker_without_writes(db_path, token, harness, sentinel)
    except Exception:
        # This file was created exclusively above inside an otherwise-empty validated profile.
        for suffix in ("", "-wal", "-shm", "-journal"):
            try:
                Path(str(db_path) + suffix).unlink()
            except FileNotFoundError:
                pass
        raise


def _same_file_identity(before: os.stat_result, after: os.stat_result) -> bool:
    return (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino)


def _fsync_file(path: Path) -> None:
    with path.open("r+b") as handle:
        os.fsync(handle.fileno())


def _publish_backup_durably(source: Path, destination: Path, profile: Path) -> None:
    if os.name == "nt":
        # os.replace() is atomic but does not request write-through on Windows. The backup pathname
        # must be durable before the DELETE transaction commits, including across sudden power loss.
        import ctypes
        from ctypes import wintypes

        move_file_ex = ctypes.WinDLL("kernel32", use_last_error=True).MoveFileExW
        move_file_ex.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
        move_file_ex.restype = wintypes.BOOL
        movefile_replace_existing = 0x1
        movefile_write_through = 0x8
        if not move_file_ex(
            str(source),
            str(destination),
            movefile_replace_existing | movefile_write_through,
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        return

    os.replace(source, destination)
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    directory_fd = os.open(profile, directory_flags)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def backup_and_clear(
    profile: Path,
    token: str,
    harness: str,
    sentinel: dict[str, Any],
) -> Path:
    db_path, initial_stat = _validate_database_file(profile)
    _verify_marker_without_writes(db_path, token, harness, sentinel)

    writer: sqlite3.Connection | None = None
    source: sqlite3.Connection | None = None
    destination: sqlite3.Connection | None = None
    temporary_backup: Path | None = None
    backup_path = Path(str(db_path) + ".pre-clear.bak")
    try:
        writer = sqlite3.connect(db_path, timeout=30, isolation_level=None)
        writer.execute("PRAGMA busy_timeout=30000")
        writer.execute("BEGIN IMMEDIATE")
        _verify_marker_on_connection(writer, token, harness, sentinel)

        current_stat = db_path.stat()
        if not _same_file_identity(initial_stat, current_stat):
            raise SafetyRefusal("the SQLite database changed identity during containment validation")
        if db_path.is_symlink() or db_path.resolve(strict=True).parent != profile:
            raise SafetyRefusal("the SQLite database became a path alias during containment validation")

        temp_fd, temp_name = tempfile.mkstemp(
            prefix=f".{DB_NAME}.backup-", suffix=".tmp", dir=profile
        )
        os.close(temp_fd)
        temporary_backup = Path(temp_name)

        # The writer's reserved lock excludes concurrent commits without changing the database. A
        # separate read-only connection feeds SQLite's online backup API, including committed WAL
        # pages, before the writer performs any DELETE.
        source = sqlite3.connect(db_path.resolve(strict=True).as_uri() + "?mode=ro", uri=True, timeout=30)
        destination = sqlite3.connect(temporary_backup, timeout=30)
        source.backup(destination)
        integrity = destination.execute("PRAGMA integrity_check").fetchall()
        if integrity != [("ok",)]:
            raise sqlite3.DatabaseError(f"backup integrity_check failed: {integrity!r}")
        destination.close()
        destination = None
        source.close()
        source = None
        _fsync_file(temporary_backup)
        _publish_backup_durably(temporary_backup, backup_path, profile)
        temporary_backup = None

        tables = {
            row[0]
            for row in writer.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()
        }
        for table in TABLES:
            if table in tables:
                writer.execute(f'DELETE FROM "{table}"')
                print(f"Cleared table: {table}")
        writer.commit()
    except Exception:
        if writer is not None and writer.in_transaction:
            writer.rollback()
        raise
    finally:
        if destination is not None:
            destination.close()
        if source is not None:
            source.close()
        if writer is not None:
            writer.close()
        if temporary_backup is not None:
            try:
                temporary_backup.unlink()
            except FileNotFoundError:
                pass

    return backup_path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--yes", action="store_true", help="confirm destructive table clearing")
    parser.add_argument(
        "--initialize-test-profile",
        action="store_true",
        help="create the SQLite marker in a brand-new harness-minted profile",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        profile, token, harness, sentinel = validate_disposable_profile()
        if args.initialize_test_profile:
            if args.yes:
                raise SafetyRefusal("initialization and destructive clearing are separate operations")
            initialize_test_profile(profile, token, harness, sentinel)
            print(f"Initialized disposable SQLite profile: {profile / DB_NAME}")
            return 0

        confirmed = args.yes or os.environ.get("CORTEX_DB_CLEAR_CONFIRM") == "1"
        if not confirmed:
            raise SafetyRefusal(
                "explicit confirmation is required (--yes or CORTEX_DB_CLEAR_CONFIRM=1)"
            )
        backup = backup_and_clear(profile, token, harness, sentinel)
        print(f"Snapshot saved: {backup}")
        print("Database cleared successfully.")
        return 0
    except SafetyRefusal as exc:
        print(f"REFUSING to clear database: {exc}", file=sys.stderr)
        return 2
    except (OSError, sqlite3.Error) as exc:
        print(f"ABORT: database was not cleared ({exc}).", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
