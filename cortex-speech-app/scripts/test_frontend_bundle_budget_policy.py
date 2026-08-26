"""Fail-closed source gate for the initial frontend bundle budget and lazy workspaces."""

import json
import random
import subprocess
import tempfile
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def test_production_build_enforces_the_manifest_budget() -> None:
    package = json.loads(read(REPO / "package.json"))
    build = package["scripts"].get("build", "")
    if "vite build && node scripts/check_bundle_budget.mjs" not in build:
        raise AssertionError("npm build must fail when the bundle budget fails")

    vite = read(REPO / "vite.config.ts")
    if "manifest: true" not in vite:
        raise AssertionError("bundle proof requires Vite's static dependency manifest")


def test_budget_counts_every_static_initial_dependency() -> None:
    gate = read(REPO / "scripts" / "check_bundle_budget.mjs")
    for required in (
        "const JAVASCRIPT_LIMIT_BYTES = 125_000",
        "const CSS_LIMIT_BYTES = 15_000",
        "for (const dependency of item.imports ?? []) visit(dependency)",
        "for (const cssFile of item.css ?? []) cssFiles.add(cssFile)",
        "gzipSync",
        "throw new Error(`Bundle budget failed:",
    ):
        if required not in gate:
            raise AssertionError(f"bundle gate lost a required fail-closed invariant: {required}")
    if "dynamicImports" in gate:
        raise AssertionError("initial budget must not mislabel on-demand dynamic workspaces as startup code")


def test_secondary_workspaces_are_literal_dynamic_imports_only() -> None:
    root = read(REPO / "src" / "App.svelte")
    if "import Workstation from './Workstation.svelte';" not in root or "<Workstation />" not in root:
        raise AssertionError("App.svelte must statically compose the Workstation source audited below")
    if len(root.splitlines()) > 350:
        raise AssertionError("App.svelte exceeded the 350-line composition-shell ceiling")

    app = read(REPO / "src" / "Workstation.svelte")
    components = (
        "SettingsPanel",
        "RefineryPanel",
        "ReviewMode",
        "KeyboardShortcuts",
        "ValidationPanel",
        "ReviewInbox",
        "SpeakerPanel",
        "DatasetMerge",
        "WslConsolePanel",
        "CommandPalette",
    )
    for component in components:
        literal = f"import('./lib/{component}.svelte')"
        if app.count(literal) != 1:
            raise AssertionError(f"{component} must have one analyzable dynamic import")
        if f"import {component} from './lib/{component}.svelte'" in app:
            raise AssertionError(f"{component} returned to the initial static graph")


def test_lazy_boundary_is_retryable_and_scrubs_internal_errors() -> None:
    boundary = read(REPO / "src" / "lib" / "LazyComponent.svelte")
    for required in (
        "typeof module.default !== 'function'",
        "currentAttempt !== activeAttempt",
        "loadFailed = true",
        "function retry()",
        "role=\"alert\"",
        "aria-busy={!loadFailed}",
    ):
        if required not in boundary:
            raise AssertionError(f"lazy boundary lost required behavior: {required}")
    if "{cause}" in boundary or "{loadError}" in boundary:
        raise AssertionError("raw dynamic-loader errors must never enter the user interface")


def test_preview_and_e2e_mocks_fail_loudly_on_unknown_commands() -> None:
    preview = read(REPO / "src" / "main.ts")
    e2e = read(REPO / "e2e" / "helpers" / "tauri-mock.ts")
    for source, name, failure in (
        (preview, "preview", "Unknown development mock command"),
        (e2e, "E2E", "Unknown E2E Tauri mock command"),
    ):
        for command in (
            "get_review_page_v1",
            "get_review_draft_v1",
        ):
            if command not in source:
                raise AssertionError(f"{name} mock lacks the typed review contract: {command}")
        if failure not in source:
            raise AssertionError(f"{name} mock silently accepts unknown commands")
    if "listKinds" in preview or "objKinds" in preview:
        raise AssertionError("preview mock must use exact command sets, not regex catch-alls")


def test_budget_executable_passes_small_graph_and_rejects_large_static_dependency() -> None:
    gate = REPO / "scripts" / "check_bundle_budget.mjs"
    with tempfile.TemporaryDirectory(prefix="cortex-bundle-budget-") as raw_temp:
        dist = Path(raw_temp)
        (dist / ".vite").mkdir()
        (dist / "assets").mkdir()
        (dist / "assets" / "entry.js").write_text("import './dependency.js';", encoding="utf-8")
        dependency = dist / "assets" / "dependency.js"
        dependency.write_text("export const ready = true;", encoding="utf-8")
        manifest = {
            "index.html": {
                "file": "assets/entry.js",
                "isEntry": True,
                "imports": ["_dependency.js"],
            },
            "_dependency.js": {"file": "assets/dependency.js"},
        }
        (dist / ".vite" / "manifest.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )

        passed = subprocess.run(
            ["node", str(gate), str(dist)],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=False,
        )
        if passed.returncode != 0 or "Bundle budget passed." not in passed.stdout:
            raise AssertionError(f"small bundle graph did not pass:\n{passed.stdout}\n{passed.stderr}")

        dependency.write_bytes(random.Random(1010).randbytes(140_000))
        failed = subprocess.run(
            ["node", str(gate), str(dist)],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=False,
        )
        if failed.returncode == 0:
            raise AssertionError("oversized transitive static dependency passed the bundle gate")
        if "initial JavaScript exceeds its limit" not in failed.stderr:
            raise AssertionError(f"oversized failure was not actionable:\n{failed.stdout}\n{failed.stderr}")


def main() -> None:
    test_production_build_enforces_the_manifest_budget()
    test_budget_counts_every_static_initial_dependency()
    test_secondary_workspaces_are_literal_dynamic_imports_only()
    test_lazy_boundary_is_retryable_and_scrubs_internal_errors()
    test_preview_and_e2e_mocks_fail_loudly_on_unknown_commands()
    test_budget_executable_passes_small_graph_and_rejects_large_static_dependency()
    print("frontend bundle-budget architecture policy passed")


if __name__ == "__main__":
    main()
