"""Regenerate the Specta IPC contract in isolation and reject tracked drift."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
MANIFEST = REPO_ROOT / "src-tauri" / "Cargo.toml"
TRACKED = REPO_ROOT / "src" / "lib" / "generated" / "ipc.ts"


def _generation_blocker() -> str | None:
    """Name the missing build input, or None when regeneration can actually run.

    `cargo run` here executes Tauri's build script, which hard-fails unless every bundled
    resource exists — and the model binaries are deliberately gitignored (fetched on the
    release workstation, absent from any fresh clone or the Linux/macOS CI checkouts).
    A skip must name that exact precondition; the Windows Release Gate machine has the
    toolchain and the models, so the drift check still bites where the exe is built.
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
            timeout=300,
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
