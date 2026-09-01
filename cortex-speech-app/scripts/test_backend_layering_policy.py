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


TEST_MODULE = re.compile(r"\n#\[cfg\(test\)\]\s*\nmod\s+\w+\s*\{")


def _end_of_block(source: str, start: int) -> int:
    """Index just past the `}` matching the `{` at `start`.

    Braces inside comments, strings, raw strings and char literals do not count. The `'` case has
    to tell a char literal from a lifetime (`'a`), which is why it is not a plain skip.
    """
    depth = 0
    i, n = start, len(source)
    while i < n:
        ch = source[i]
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            nl = source.find("\n", i)
            i = n if nl < 0 else nl + 1
            continue
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            end = source.find("*/", i + 2)
            i = n if end < 0 else end + 2
            continue
        if ch == "r" and i + 1 < n and source[i + 1] in '"#':
            j = i + 1
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"':
                terminator = '"' + "#" * hashes
                end = source.find(terminator, j + 1)
                i = n if end < 0 else end + len(terminator)
                continue
        if ch == '"':
            i += 1
            while i < n:
                if source[i] == "\\":
                    i += 2
                    continue
                if source[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if ch == "'":
            if i + 2 < n and source[i + 1] == "\\":  # '\n', '\'' …
                end = source.find("'", i + 2)
                i = n if end < 0 else end + 1
            elif i + 2 < n and source[i + 2] == "'":  # 'x'
                i += 3
            else:  # a lifetime, not a literal
                i += 1
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def production_prefix(source: str) -> str:
    """The source with every `#[cfg(test)]` module removed — production code only.

    This used to cut the file at the first `#[cfg(test)] mod tests {` and scan only the prefix,
    which was wrong twice over (both measured 2026-09-01):

    1. The module NAME is not fixed. Alongside `mod tests` this repo has
       `mod state_command_surface_tests`, `mod system_ops_boundary_tests`,
       `mod typed_ingest_refusal_and_identity_tests` and others. Those files were scanned in
       FULL, so a test legitimately opening a connection read as the command layer regaining SQL
       authority — which cost a real test, deleted rather than dodge this gate.
    2. Several files interleave test modules with production code, and everything after the
       first match went unscanned. Measured, the escaped production code is small — 2
       `#[tauri::command]` items across 2 files, no top-level `pub fn` — because the bulk of
       every truncated tail is test code that is exempt anyway. Small, but it is exactly the
       code this gate names, and unscanned is unscanned.

    Removing each test module by brace matching, rather than truncating at the first one, keeps
    the test code exempt without blinding the gate to what follows it.
    """
    out: list[str] = []
    i = 0
    while True:
        marker = TEST_MODULE.search(source, i)
        if marker is None:
            out.append(source[i:])
            return "".join(out)
        out.append(source[i : marker.start()])
        i = _end_of_block(source, marker.end() - 1)


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
    command_files = [RUST / "commands.rs", *sorted((RUST / "commands").glob("*.rs"))]
    commands = "\n".join(production_prefix(path.read_text(encoding="utf-8")) for path in command_files)
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
