"""Monotonic backend layering guard for command, store, restore, and backup boundaries.

This is not the final module-size certificate: the explicit db/Couch/command decomposition debt is
still open. It prevents completed strangler slices from leaking SQL or UI dependencies back across
their boundaries while that remaining decomposition proceeds.
"""

from __future__ import annotations

import re
from pathlib import Path


APP = Path(__file__).resolve().parents[1]
RUST = APP / "src-tauri" / "src"


def production_prefix(source: str) -> str:
    marker = re.search(r"\n#\[cfg\(test\)\]\s*\nmod tests\s*\{", source)
    return source[: marker.start()] if marker else source


def test_commands_do_not_issue_sql_or_receive_raw_connections() -> None:
    command_files = [RUST / "commands.rs", *sorted((RUST / "commands").glob("*.rs"))]
    forbidden = ("rusqlite::", ".connection()", ".query_row(", ".prepare(", ".execute(")
    violations: list[str] = []
    for path in command_files:
        source = production_prefix(path.read_text(encoding="utf-8"))
        for token in forbidden:
            if token in source:
                violations.append(f"{path.relative_to(RUST)} contains {token}")
    if violations:
        raise AssertionError("command layer regained SQL authority:\n" + "\n".join(violations))


def test_backend_services_and_stores_remain_ui_independent() -> None:
    service_files = [
        RUST / "backup_service.rs",
        *sorted((RUST / "restore_service").glob("*.rs")),
        *sorted((RUST / "stores").glob("*.rs")),
    ]
    forbidden = ("use tauri", "tauri::", "crate::commands", "crate::http", "crate::AppState")
    violations: list[str] = []
    for path in service_files:
        source = production_prefix(path.read_text(encoding="utf-8"))
        for token in forbidden:
            if token in source:
                violations.append(f"{path.relative_to(RUST)} contains {token}")
    if violations:
        raise AssertionError("backend service crossed into UI/HTTP authority:\n" + "\n".join(violations))


def test_backup_command_delegates_artifact_verification_to_the_service() -> None:
    commands = production_prefix((RUST / "commands.rs").read_text(encoding="utf-8"))
    start = commands.find("pub async fn db_backup(")
    end = commands.find("\n#[tauri::command]", start + 1)
    if start < 0 or end < 0:
        raise AssertionError("db_backup command boundary is missing")
    body = commands[start:end]
    for required in (
        "database.open_read()",
        "backup_db.backup(&validated)",
        "backup_service::verify_backup_file",
    ):
        if required not in body:
            raise AssertionError(f"db_backup lost service delegation: {required}")
    for forbidden in ("rusqlite::", ".query_row(", "PRAGMA integrity_check", "SELECT COUNT"):
        if forbidden in body:
            raise AssertionError(f"db_backup regained inline SQL verification: {forbidden}")


def main() -> None:
    test_commands_do_not_issue_sql_or_receive_raw_connections()
    test_backend_services_and_stores_remain_ui_independent()
    test_backup_command_delegates_artifact_verification_to_the_service()
    print("backend layering policy passed")


if __name__ == "__main__":
    main()
