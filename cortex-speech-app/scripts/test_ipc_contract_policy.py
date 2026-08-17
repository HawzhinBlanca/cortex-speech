"""IPC contract gate (P3.1 — closes T1; auto-discovered by run_python_policies.py).

Nothing else diffs the frontend `invoke('name')` call sites against the AUTHORITATIVE Rust command
registry — the `tauri::generate_handler![...]` list in lib.rs. So a renamed or removed `#[tauri::command]`
stays green in vitest + Playwright + cargo simultaneously: vitest mocks `@tauri-apps/api/core`'s invoke,
the Playwright tauri-mock's default branch returns `null` for any unknown command, and cargo never sees
the frontend — the dangling call only fails at runtime, as an error toast on a real user's machine.

This gate is a generated CONTRACT, not an allow-list: it parses BOTH real sources and FAILS if the
frontend invokes a command name the registry does not export. Registered-but-never-invoked commands are
reported as INFO only (many are legitimately reached from events/tests/other surfaces, or reserved).

Dynamic `invoke(expr)` calls (a name built at runtime, not a string literal) cannot be resolved statically;
they are reported so a reviewer knows they are NOT contract-checked, but they do not fail the gate.
"""

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LIB_RS = REPO_ROOT / "src-tauri" / "src" / "lib.rs"
FRONTEND_DIR = REPO_ROOT / "src"

# Any `invoke(` opening; the first-arg char right after decides literal vs dynamic (see below).
_INVOKE_OPEN = re.compile(r"\binvoke\s*(?:<[^>]*>)?\s*\(\s*")


from _policy_util import strip_comments as _strip_comments  # noqa: E402


def registered_commands() -> set[str]:
    """The authoritative command names from `tauri::generate_handler![...]` in lib.rs. Each entry is a
    path to a `#[tauri::command]` fn (`commands::foo`, `commands::jury::bar`); Tauri registers it under the
    fn name (the last `::` segment) — no `#[tauri::command(rename=...)]` is used in this repo (pinned below)."""
    # Strip comments FIRST so a `[`/`]` ever placed inside a handler-block comment cannot corrupt the
    # bracket-depth match (the block already carries `// Phase N` comments; a future bracketed one must not
    # break the parse).
    text = _strip_comments(LIB_RS.read_text(encoding="utf-8"))
    idx = text.find("generate_handler![")
    if idx == -1:
        raise AssertionError("generate_handler![ not found in lib.rs — the IPC contract cannot be built")
    start = text.find("[", idx)
    depth, i = 0, start
    while i < len(text):
        if text[i] == "[":
            depth += 1
        elif text[i] == "]":
            depth -= 1
            if depth == 0:
                break
        i += 1
    block = text[start + 1 : i]
    names = {entry.strip().split("::")[-1] for entry in block.split(",") if entry.strip()}
    if not names:
        raise AssertionError("no commands parsed from generate_handler![...] — the contract would be vacuous")
    return names


def frontend_invocations() -> tuple[set[str], list[str]]:
    """(app-command names invoked from the frontend as string literals, dynamic-invoke site descriptions).

    Classify EVERY `invoke(` by its first-arg char so a quoted-but-odd name cannot fall through both paths
    (the tight-regex gap an adversary found): a `'`/`"`/backtick literal is extracted WHOLE — spaces, a
    hyphen, a typo included — and checked against the registry (so `invoke('get_segments ')` is flagged, not
    silently dropped); a backtick with `${...}` interpolation, or a non-quote first arg (a variable), is a
    runtime name reported as dynamic (not statically checkable). Plugin/core commands (`plugin:dialog|open`
    — they carry `:`/`|` and are registered by the plugin, not generate_handler!) are skipped, not flagged."""
    literals: set[str] = set()
    dynamic: list[str] = []
    files = sorted(FRONTEND_DIR.rglob("*.ts")) + sorted(FRONTEND_DIR.rglob("*.svelte"))
    for path in files:
        code = _strip_comments(path.read_text(encoding="utf-8"))
        for m in _INVOKE_OPEN.finditer(code):
            pos = m.end()
            quote = code[pos : pos + 1]
            site = f"{path.relative_to(REPO_ROOT).as_posix()}:{code[:m.start()].count(chr(10)) + 1}"
            if quote in ("'", '"', "`"):
                end = code.find(quote, pos + 1)
                if end == -1:
                    dynamic.append(site)  # unterminated literal — can't extract a name
                    continue
                name = code[pos + 1 : end]
                if quote == "`" and "${" in name:
                    dynamic.append(site)  # interpolated template — the name is built at runtime
                elif ":" in name or "|" in name:
                    continue  # a plugin/core command (plugin:dialog|open) — not a generate_handler! command
                else:
                    literals.add(name)
            else:
                dynamic.append(site)  # first arg is a variable / expression — a runtime name
    return literals, dynamic


def test_no_command_rename_attribute() -> None:
    # The contract maps a registry entry to its fn name (last path segment). A `#[tauri::command(rename =
    # ...)]` would register a DIFFERENT name, silently breaking that mapping — pin that none exist.
    for rs in (REPO_ROOT / "src-tauri" / "src").rglob("*.rs"):
        text = rs.read_text(encoding="utf-8")
        if re.search(r"#\[tauri::command\([^)]*\brename\b", text) or re.search(r"#\[tauri::command\([^)]*\bname\s*=", text):
            raise AssertionError(
                f"{rs.name} uses #[tauri::command(rename/name=...)]; the IPC contract maps registry entries to "
                "their fn name (last :: segment). Update registered_commands() to honor the rename."
            )


def test_frontend_invokes_only_registered_commands() -> None:
    registry = registered_commands()
    invoked, dynamic = frontend_invocations()
    if not invoked:
        raise AssertionError(
            "no invoke('...') string-literal call sites found in the frontend — the contract would be vacuous "
            "(did the invoke wrapper move, or the parser break?)"
        )
    dangling = sorted(invoked - registry)
    if dangling:
        raise AssertionError(
            f"frontend invoke() calls a command the Rust registry (generate_handler! in lib.rs) does NOT "
            f"export: {dangling}. A renamed/removed #[tauri::command] leaves a DANGLING invoke that fails only "
            "at runtime (vitest mocks invoke; the Playwright tauri-mock returns null for unknown commands). "
            "Rename the frontend call to match the registry, or restore/register the command."
        )
    # INFO (never fails): coverage of the registry + any un-checkable dynamic invokes.
    uninvoked = sorted(registry - invoked)
    print(
        f"ipc contract: {len(invoked)} invoked / {len(registry)} registered; "
        f"{len(uninvoked)} registered-but-not-invoked (info); {len(dynamic)} dynamic invoke(s) not statically checked"
    )
    if dynamic:
        print("  dynamic invoke sites (NOT contract-checked): " + ", ".join(dynamic))


def main() -> None:
    test_no_command_rename_attribute()
    test_frontend_invokes_only_registered_commands()
    print("ipc contract policy regression passed")


if __name__ == "__main__":
    main()
