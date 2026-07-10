"""Clear the app's working tables — DANGEROUS: only ever against a DISPOSABLE profile.

Safety contract (a reliability test must NEVER be able to erase the owner's real library):
  * Refuses to run without explicit confirmation (--yes or CORTEX_DB_CLEAR_CONFIRM=1).
  * Snapshots the DB to <db>.pre-clear.bak BEFORE deleting, and aborts if the snapshot fails —
    so a clear is always recoverable.
  * Honors CORTEX_APP_DATA_DIR (point it at a throwaway profile) before falling back to %APPDATA%.
"""
import os
import shutil
import sqlite3
import sys

TABLES = [
    "speech_segments",
    "segment_hypotheses",
    "segment_edits",
    "dataset_runs",
    "agent_stage_events",
    "agent_import_reports",
]


def resolve_db_path() -> str:
    base = os.environ.get("CORTEX_APP_DATA_DIR")
    if base:
        return os.path.join(base, "cortex-speech.db")
    return os.path.expandvars(r"%APPDATA%\cortex-speech\cortex-speech.db")


def main() -> int:
    db_path = resolve_db_path()

    confirmed = os.environ.get("CORTEX_DB_CLEAR_CONFIRM") == "1" or "--yes" in sys.argv
    if not confirmed:
        print(
            "REFUSING to clear the database without explicit confirmation.\n"
            f"  target: {db_path}\n"
            "  This DELETEs every imported segment, edit, hypothesis, and run — it can erase your\n"
            "  REAL library. Re-run with --yes (or CORTEX_DB_CLEAR_CONFIRM=1), and ideally point\n"
            "  CORTEX_APP_DATA_DIR at a DISPOSABLE profile first.",
            file=sys.stderr,
        )
        return 2

    if not os.path.exists(db_path):
        print("Database file does not exist yet.")
        return 0

    # Snapshot FIRST — a clear must always be recoverable. Abort (do not delete) if it fails.
    backup = db_path + ".pre-clear.bak"
    try:
        shutil.copy2(db_path, backup)
    except OSError as e:
        print(f"ABORT: could not snapshot the database before clearing ({e}).", file=sys.stderr)
        return 1
    print(f"Snapshot saved: {backup}")

    conn = sqlite3.connect(db_path)
    try:
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table';")
        tables = {row[0] for row in cursor.fetchall()}
        for table in TABLES:
            if table in tables:
                cursor.execute(f"DELETE FROM {table};")
                print(f"Cleared table: {table}")
        conn.commit()
    finally:
        conn.close()
    print("Database cleared successfully.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
