"""Destructive containment regressions for the E2E database-clear harness."""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path


APP_ROOT = Path(__file__).resolve().parents[1]
CLEAR_DB = APP_ROOT / "clear_db.py"
PROFILE_HELPER = APP_ROOT / "e2e_profile.cjs"
SENTINEL_NAME = ".cortex-e2e-disposable.json"
PURPOSE = "cortex-e2e-disposable-profile"
HARNESS = "clear_db_containment_test"


def _application_id(token: str) -> int:
    return (int(token[:8], 16) & 0x7FFFFFFF) or 1


def _contract_env(profile: Path, token: str, harness: str = HARNESS) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "CORTEX_APP_DATA_DIR": str(profile),
            "CORTEX_TEST_PROFILE_TOKEN": token,
            "CORTEX_TEST_PROFILE_HARNESS": harness,
            "CORTEX_DB_CLEAR_CONFIRM": "1",
            "PYTHON": sys.executable,
        }
    )
    return env


def _run_clear(profile: Path, token: str, *args: str, harness: str = HARNESS) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CLEAR_DB), *args],
        cwd=APP_ROOT,
        env=_contract_env(profile, token, harness),
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )


def _sentinel(profile: Path, token: str, harness: str = HARNESS) -> None:
    value = {
        "schema": 1,
        "purpose": PURPOSE,
        "profileToken": token,
        "sqliteApplicationId": _application_id(token),
        "harness": harness,
        "canonicalProfile": str(profile.resolve()),
        "createdAtUtc": "2026-08-26T00:00:00.000Z",
    }
    (profile / SENTINEL_NAME).write_text(json.dumps(value) + "\n", encoding="utf-8")


def _create_wal_database(
    profile: Path,
    token: str,
    *,
    include_marker: bool,
    harness: str = HARNESS,
) -> sqlite3.Connection:
    db_path = profile / "cortex-speech.db"
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA wal_autocheckpoint=0")
    if include_marker:
        conn.execute(f"PRAGMA application_id={_application_id(token)}")
    conn.execute("CREATE TABLE speech_segments (id TEXT PRIMARY KEY, raw_transcript TEXT)")
    conn.execute("INSERT INTO speech_segments VALUES ('checkpointed', 'main-db row')")
    conn.commit()
    # Make the sentinel marker and first data row visible to immutable validation in the main file.
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    # This second committed row remains in WAL while the connection stays open. A file copy of only
    # cortex-speech.db would omit it; SQLite backup() must include it.
    conn.execute("INSERT INTO speech_segments VALUES ('wal-only', 'committed WAL row')")
    conn.commit()
    wal_path = Path(str(db_path) + "-wal")
    if not wal_path.exists() or wal_path.stat().st_size == 0:
        raise AssertionError("test setup did not leave a committed row in WAL")
    return conn


def _db_and_wal_bytes(profile: Path) -> tuple[bytes, bytes]:
    db_path = profile / "cortex-speech.db"
    wal_path = Path(str(db_path) + "-wal")
    return db_path.read_bytes(), wal_path.read_bytes()


def _mint_profile() -> dict[str, object]:
    env = os.environ.copy()
    for key in (
        "CORTEX_APP_DATA_DIR",
        "CORTEX_TEST_PROFILE_TOKEN",
        "CORTEX_TEST_PROFILE_HARNESS",
    ):
        env.pop(key, None)
    env["PYTHON"] = sys.executable
    js = (
        f"const h=require({json.dumps(str(PROFILE_HELPER))});"
        f"process.stdout.write(JSON.stringify(h.resolveDisposableProfile({json.dumps(HARNESS)})));"
    )
    result = subprocess.run(
        ["node", "-e", js],
        cwd=APP_ROOT,
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(f"profile helper failed: {result.stdout}\n{result.stderr}")
    minted = json.loads(result.stdout)
    profile = Path(str(minted["dataDir"]))
    token = str(minted["profileToken"])
    conn = sqlite3.connect(profile / "cortex-speech.db")
    try:
        objects = conn.execute(
            "SELECT COUNT(*) FROM sqlite_schema "
            "WHERE name NOT LIKE 'sqlite_%' AND type IN ('table', 'view', 'trigger', 'index')"
        ).fetchone()
        if objects != (0,):
            raise AssertionError("SQLite safety marker made the app bootstrap database non-pristine")
        if conn.execute("PRAGMA application_id").fetchone() != (_application_id(token),):
            raise AssertionError("SQLite safety marker does not match the minted token")
    finally:
        conn.close()
    return minted


def _remove_minted_profile(profile: Path) -> None:
    canonical = profile.resolve(strict=True)
    temp_root = Path(tempfile.gettempdir()).resolve(strict=True)
    if canonical.parent != temp_root or not canonical.name.startswith("cortex-e2e-"):
        raise AssertionError(f"refusing test cleanup outside the minted temp boundary: {canonical}")
    shutil.rmtree(canonical)


def _remove_repo_fixture(profile: Path) -> None:
    canonical = profile.resolve(strict=True)
    if canonical.parent != APP_ROOT or not canonical.name.startswith(".clear-db-containment-"):
        raise AssertionError(f"refusing test cleanup outside the exact repo fixture boundary: {canonical}")
    shutil.rmtree(canonical)


class ClearDbContainmentTests(unittest.TestCase):
    def test_relocated_profile_is_refused_even_with_forged_sentinel_and_marker(self) -> None:
        profile = APP_ROOT / f".clear-db-containment-relocated-{uuid.uuid4().hex}"
        profile.mkdir()
        token = uuid.uuid4().hex + uuid.uuid4().hex
        conn: sqlite3.Connection | None = None
        try:
            _sentinel(profile, token)
            conn = _create_wal_database(profile, token, include_marker=True)
            before = _db_and_wal_bytes(profile)

            clear = _run_clear(profile, token, "--yes")
            self.assertEqual(clear.returncode, 2, clear.stdout + clear.stderr)
            self.assertIn("not a direct harness-minted", clear.stderr)
            self.assertEqual(_db_and_wal_bytes(profile), before)
            self.assertFalse(Path(str(profile / "cortex-speech.db") + ".pre-clear.bak").exists())

            env = os.environ.copy()
            env["CORTEX_APP_DATA_DIR"] = str(profile)
            env["PYTHON"] = sys.executable
            js = (
                f"const h=require({json.dumps(str(PROFILE_HELPER))});"
                f"h.resolveDisposableProfile({json.dumps(HARNESS)});"
            )
            helper = subprocess.run(
                ["node", "-e", js],
                cwd=APP_ROOT,
                env=env,
                text=True,
                capture_output=True,
                timeout=30,
                check=False,
            )
            self.assertNotEqual(helper.returncode, 0)
            self.assertIn("caller-supplied CORTEX_APP_DATA_DIR", helper.stderr)
            self.assertEqual(_db_and_wal_bytes(profile), before)
        finally:
            if conn is not None:
                conn.close()
            if profile.exists():
                _remove_repo_fixture(profile)

    def test_valid_sentinel_without_sqlite_marker_refuses_byte_identically(self) -> None:
        minted = _mint_profile()
        profile = Path(str(minted["dataDir"]))
        token = str(minted["profileToken"])
        db_path = profile / "cortex-speech.db"
        conn: sqlite3.Connection | None = None
        try:
            db_path.unlink()
            conn = _create_wal_database(profile, token, include_marker=False)
            before = _db_and_wal_bytes(profile)
            result = _run_clear(profile, token, "--yes")
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("SQLite test-profile marker", result.stderr)
            self.assertEqual(_db_and_wal_bytes(profile), before)
            self.assertFalse(Path(str(db_path) + ".pre-clear.bak").exists())
        finally:
            if conn is not None:
                conn.close()
            _remove_minted_profile(profile)

    def test_initialization_cannot_bless_an_existing_database(self) -> None:
        profile = Path(tempfile.mkdtemp(prefix="cortex-e2e-"))
        token = uuid.uuid4().hex + uuid.uuid4().hex
        conn: sqlite3.Connection | None = None
        try:
            _sentinel(profile, token)
            conn = _create_wal_database(profile, token, include_marker=False)
            before = _db_and_wal_bytes(profile)
            result = _run_clear(profile, token, "--initialize-test-profile")
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("brand-new directory", result.stderr)
            self.assertEqual(_db_and_wal_bytes(profile), before)
        finally:
            if conn is not None:
                conn.close()
            _remove_minted_profile(profile)

    def test_successful_clear_uses_wal_consistent_sqlite_backup(self) -> None:
        minted = _mint_profile()
        profile = Path(str(minted["dataDir"]))
        token = str(minted["profileToken"])
        conn: sqlite3.Connection | None = None
        try:
            conn = sqlite3.connect(profile / "cortex-speech.db")
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA wal_autocheckpoint=0")
            conn.execute("CREATE TABLE speech_segments (id TEXT PRIMARY KEY, raw_transcript TEXT)")
            conn.execute("INSERT INTO speech_segments VALUES ('checkpointed', 'main-db row')")
            conn.commit()
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            conn.execute("INSERT INTO speech_segments VALUES ('wal-only', 'committed WAL row')")
            conn.commit()
            self.assertGreater(Path(str(profile / "cortex-speech.db") + "-wal").stat().st_size, 0)

            result = _run_clear(profile, token, "--yes")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(
                conn.execute("SELECT id FROM speech_segments ORDER BY id").fetchall(),
                [],
            )
            self.assertEqual(conn.execute("PRAGMA application_id").fetchone(), (_application_id(token),))

            backup = sqlite3.connect(Path(str(profile / "cortex-speech.db") + ".pre-clear.bak"))
            try:
                self.assertEqual(
                    backup.execute("SELECT id FROM speech_segments ORDER BY id").fetchall(),
                    [("checkpointed",), ("wal-only",)],
                )
                self.assertEqual(backup.execute("PRAGMA integrity_check").fetchone(), ("ok",))
                self.assertEqual(
                    backup.execute("PRAGMA application_id").fetchone(),
                    (_application_id(token),),
                )
            finally:
                backup.close()
        finally:
            if conn is not None:
                conn.close()
            _remove_minted_profile(profile)

    def test_junction_alias_to_external_profile_is_refused_byte_identically(self) -> None:
        target = APP_ROOT / f".clear-db-containment-junction-{uuid.uuid4().hex}"
        alias = Path(tempfile.gettempdir()) / f"cortex-e2e-alias-{uuid.uuid4().hex}"
        target.mkdir()
        token = uuid.uuid4().hex + uuid.uuid4().hex
        conn: sqlite3.Connection | None = None
        alias_created = False
        try:
            _sentinel(target, token)
            conn = _create_wal_database(target, token, include_marker=True)
            before = _db_and_wal_bytes(target)
            if os.name == "nt":
                made = subprocess.run(
                    ["cmd.exe", "/d", "/c", "mklink", "/J", str(alias), str(target)],
                    text=True,
                    capture_output=True,
                    timeout=15,
                    check=False,
                )
                self.assertEqual(made.returncode, 0, made.stdout + made.stderr)
            else:
                alias.symlink_to(target, target_is_directory=True)
            alias_created = True
            self.assertEqual(alias.resolve(strict=True), target.resolve(strict=True))

            result = _run_clear(alias, token, "--yes")
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("path alias", result.stderr)
            self.assertEqual(_db_and_wal_bytes(target), before)
            self.assertFalse(Path(str(target / "cortex-speech.db") + ".pre-clear.bak").exists())
        finally:
            if conn is not None:
                conn.close()
            if alias_created:
                if os.name == "nt":
                    os.rmdir(alias)
                else:
                    alias.unlink()
            if target.exists():
                _remove_repo_fixture(target)


if __name__ == "__main__":
    unittest.main()
