"""IPC contract gate (P3.1 — closes T1; auto-discovered by run_python_policies.py).

Nothing else diffs generated and handwritten frontend IPC call sites against the AUTHORITATIVE Rust command
registry — the `tauri::generate_handler![...]` list in lib.rs. So a renamed or removed `#[tauri::command]`
stays green in vitest + Playwright + cargo simultaneously: vitest mocks `@tauri-apps/api/core`'s invoke,
the Playwright tauri-mock's default branch returns `null` for any unknown command, and cargo never sees
the frontend — the dangling call only fails at runtime, as an error toast on a real user's machine.

This gate is a generated CONTRACT, not an allow-list: it parses BOTH real sources and FAILS if the
frontend invokes a command name the registry does not export. Registered-but-never-invoked commands are
reported as INFO only (many are legitimately reached from events/tests/other surfaces, or reserved).

Dynamic command names fail the gate. The sole low-level exception is the closed handwritten adapter:
its one `invokeDesktop(command, ...)` bridge accepts a TypeScript union sourced from an audited literal
inventory, while every service call into that bridge remains a statically named literal.
"""

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LIB_RS = REPO_ROOT / "src-tauri" / "src" / "lib.rs"
FRONTEND_DIR = REPO_ROOT / "src"
LEGACY_ADAPTER = FRONTEND_DIR / "lib" / "adapters" / "legacyIpc.ts"

# Every supported IPC boundary. The bounded non-greedy generic matcher handles multiline explicit
# result types without treating ordinary functions whose names merely contain "invoke" as IPC.
_INVOKE_OPEN = re.compile(
    r"\b(invoke|invokeLegacy|invokeCritical|invokeDesktop|__TAURI_INVOKE)"
    r"\s*(?:<[\s\S]{0,4000}?>\s*)?\(\s*"
)


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


def frontend_invocations() -> tuple[set[str], set[str], list[str]]:
    """Return (handwritten literals, generated literals, dynamic site descriptions).

    Classify EVERY `invoke(` by its first-arg char so a quoted-but-odd name cannot fall through both paths
    (the tight-regex gap an adversary found): a `'`/`"`/backtick literal is extracted WHOLE — spaces, a
    hyphen, a typo included — and checked against the registry (so `invoke('get_segments ')` is flagged, not
    silently dropped); a backtick with `${...}` interpolation, or a non-quote first arg (a variable), is a
    runtime name reported as dynamic (not statically checkable). Plugin/core commands (`plugin:dialog|open`
    — they carry `:`/`|` and are registered by the plugin, not generate_handler!) are skipped, not flagged."""
    handwritten: set[str] = set()
    generated: set[str] = set()
    dynamic: list[str] = []
    files = [
        path
        for path in sorted(FRONTEND_DIR.rglob("*.ts")) + sorted(FRONTEND_DIR.rglob("*.svelte"))
        if not path.name.endswith((".test.ts", ".spec.ts"))
    ]
    for path in files:
        code = _strip_comments(path.read_text(encoding="utf-8"))
        for m in _INVOKE_OPEN.finditer(code):
            if re.search(r"\bfunction\s*$", code[max(0, m.start() - 32) : m.start()]):
                continue  # declaration, not a call expression
            callee = m.group(1)
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
                    if callee == "__TAURI_INVOKE":
                        generated.add(name)
                    else:
                        handwritten.add(name)
            else:
                dynamic.append(site)  # first arg is a variable / expression — a runtime name
    return handwritten, generated, dynamic


def handwritten_inventory() -> set[str]:
    code = _strip_comments(LEGACY_ADAPTER.read_text(encoding="utf-8"))
    match = re.search(r"LEGACY_IPC_COMMANDS\s*=\s*\[([\s\S]*?)]\s*as\s+const", code)
    if not match:
        raise AssertionError("closed LEGACY_IPC_COMMANDS inventory is missing from legacyIpc.ts")
    names = set(re.findall(r"['\"]([a-z][a-z0-9_]*)['\"]", match.group(1)))
    if not names:
        raise AssertionError("closed handwritten IPC inventory is empty — the gate would be vacuous")
    return names


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
    handwritten, generated, dynamic = frontend_invocations()
    invoked = handwritten | generated
    inventory = handwritten_inventory()
    if not invoked:
        raise AssertionError(
            "no generated or handwritten string-literal IPC call sites found — the contract would be vacuous"
        )
    dangling = sorted(invoked - registry)
    if dangling:
        raise AssertionError(
            f"frontend invoke() calls a command the Rust registry (generate_handler! in lib.rs) does NOT "
            f"export: {dangling}. A renamed/removed #[tauri::command] leaves a DANGLING invoke that fails only "
            "at runtime (vitest mocks invoke; the Playwright tauri-mock returns null for unknown commands). "
            "Rename the frontend call to match the registry, or restore/register the command."
        )
    stale_inventory = sorted(inventory - registry)
    if stale_inventory:
        raise AssertionError(
            f"legacyIpc.ts allow-lists commands absent from the Rust registry: {stale_inventory}"
        )
    missing_inventory = sorted(handwritten - inventory)
    if missing_inventory:
        raise AssertionError(
            f"handwritten IPC calls bypass the closed legacy inventory: {missing_inventory}"
        )
    unused_inventory = sorted(inventory - handwritten)
    if unused_inventory:
        raise AssertionError(
            f"legacyIpc.ts carries unused command capabilities: {unused_inventory}; remove them"
        )
    generated_through_legacy = sorted(generated & inventory)
    if generated_through_legacy:
        raise AssertionError(
            f"generated commands regressed into the handwritten adapter: {generated_through_legacy}"
        )
    expected_dynamic_prefix = "src/lib/adapters/legacyIpc.ts:"
    unexpected_dynamic = [site for site in dynamic if not site.startswith(expected_dynamic_prefix)]
    if unexpected_dynamic or len(dynamic) != 1:
        raise AssertionError(
            "dynamic IPC names are forbidden; expected only the single closed legacy bridge, got: "
            + ", ".join(dynamic)
        )
    # INFO (never fails): coverage of the registry + any un-checkable dynamic invokes.
    uninvoked = sorted(registry - invoked)
    print(
        f"ipc contract: {len(invoked)} invoked ({len(generated)} generated, "
        f"{len(handwritten)} handwritten) / {len(registry)} registered; "
        f"{len(uninvoked)} registered-but-not-invoked (info); one closed low-level bridge"
    )


def main() -> None:
    test_no_command_rename_attribute()
    test_frontend_invokes_only_registered_commands()
    print("ipc contract policy regression passed")


if __name__ == "__main__":
    main()
