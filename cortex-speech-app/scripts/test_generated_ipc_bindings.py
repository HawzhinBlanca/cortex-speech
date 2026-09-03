"""Regenerate the Specta IPC contract in isolation and reject tracked drift."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
MANIFEST = REPO_ROOT / "src-tauri" / "Cargo.toml"
TRACKED = REPO_ROOT / "src" / "lib" / "generated" / "ipc.ts"
# Generous because this is a real build, not a lint: the Linux/macOS smoke jobs carry no cargo
# cache (only the Windows job does), so the first `cargo run` here compiles the whole dependency
# graph on a 2-core hosted runner. 300s was a Windows-warm-cache number.
GENERATION_TIMEOUT_SECONDS = 1800
# `cargo run` below builds the dev profile, so only a dev-profile artifact proves the dependency
# tree is already compiled. A release-only target dir leaves the dev build just as cold.
PREBUILT_GENERATOR = (
    Path(os.environ.get("CARGO_TARGET_DIR") or (REPO_ROOT / "src-tauri" / "target"))
    / "debug"
    / ("generate_ipc_bindings.exe" if os.name == "nt" else "generate_ipc_bindings")
)


def _generation_blocker() -> str | None:
    """Name the missing build input, or None when regeneration can actually run.

    `cargo run` here executes Tauri's build script, which hard-fails unless every bundled
    resource exists — and the model binaries are deliberately gitignored (fetched on the
    release workstation, absent from any fresh clone or the Linux/macOS CI checkouts).
    A skip must name that exact precondition; the drift check still bites on every machine
    that actually builds the crate — the release workstation and any warm checkout.
    """
    if shutil.which("cargo") is None:
        return "the cargo toolchain is not installed"
    configuration = json.loads((REPO_ROOT / "src-tauri" / "tauri.conf.json").read_text(encoding="utf-8"))
    missing = [
        resource
        for resource in configuration["bundle"]["resources"]
        if not (REPO_ROOT / "src-tauri" / resource).exists()
    ]
    if missing:
        return f"bundled build resources are absent (gitignored model binaries): {', '.join(sorted(missing))}"
    # `tauri::generate_context!` hard-fails when the frontend dist is unbuilt — a gitignored build
    # output, absent from every fresh clone (measured on Linux CI: "frontendDist ../dist doesn't
    # exist" panics the proc macro before the generator can run).
    frontend_dist = configuration["build"]["frontendDist"]
    if not (REPO_ROOT / "src-tauri" / frontend_dist).exists():
        return f"the frontend dist is not built (gitignored build output): {frontend_dist}"
    # A cold cargo cache makes this gate a compile job, not a drift check: `cargo run` would build
    # the whole Tauri dependency tree before the generator emits a byte. Measured on GitHub's
    # windows-latest runner (PR #73), that compile blew the 300s budget outright; the same command
    # against an already-built dev target here returns in ~1.5s. So require the artifact cargo
    # itself leaves behind — present on the release workstation and any warm checkout, absent on a
    # hosted runner, where the crate is not compiled until the later clippy/test steps.
    if not PREBUILT_GENERATOR.is_file():
        return (
            "the cargo dev target is cold — no prebuilt generator at "
            f"{PREBUILT_GENERATOR}, so `cargo run` would compile the Tauri dependency tree from "
            f"scratch, which does not fit this gate's {GENERATION_TIMEOUT_SECONDS}s budget"
        )
    return None


def main() -> None:
    if not TRACKED.is_file():
        raise AssertionError(f"generated IPC contract is missing: {TRACKED}")
    blocker = _generation_blocker()
    if blocker is not None:
        print(f"SKIPPED: IPC binding regeneration cannot run here — {blocker}")
        return
    with tempfile.TemporaryDirectory(prefix="cortex-ipc-") as temporary:
        generated = Path(temporary) / "ipc.ts"
        completed = subprocess.run(
            [
                "cargo",
                "run",
                "--quiet",
                "--manifest-path",
                str(MANIFEST),
                "--bin",
                "generate_ipc_bindings",
                "--",
                str(generated),
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            timeout=GENERATION_TIMEOUT_SECONDS,
        )
        if completed.returncode != 0:
            raise AssertionError(
                "IPC binding generation failed:\n" + completed.stdout + completed.stderr
            )
        expected = TRACKED.read_bytes()
        actual = generated.read_bytes()
        if actual != expected:
            raise AssertionError(
                "generated IPC bindings drifted; run the generator and commit src/lib/generated/ipc.ts"
            )
        rendered = actual.decode("utf-8")
        decisions = re.findall(r"^export type ReviewDecisionV1 = ([^;]+);$", rendered, re.MULTILINE)
        if decisions != ['"accept" | "edit" | "reject"']:
            raise AssertionError(
                "desktop ReviewDecisionV1 must advertise only accept/edit/reject; "
                "moving on without a decision is renderer navigation, never a commit payload"
            )
    print("generated IPC bindings are current")


if __name__ == "__main__":
    main()
