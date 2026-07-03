#!/usr/bin/env python3
"""P0.2 — the stale-exe guard (deep-audit F4).

Proves, WITHOUT running the app, that the built release exe (a) is newer than every source file
that feeds it and (b) was compiled from the current git HEAD. The SHA is recovered from a
contiguous `CORTEX_BUILD_SHA:<sha>` marker baked into the binary's rodata by lib.rs
(`GIT_SHA_MARKER`, forced in with `#[used]`), so no execution is needed.

This is a LOCAL ship gate, not a CI step: CI runs on Linux and never builds the Windows exe.
It is invoked by `make ship-check-local` (which runs `build-app` first, guaranteeing a fresh exe)
and can be run by hand. The pure decision logic lives in `evaluate_freshness` and is unit-tested
CI-safely by `test_exe_freshness.py` with synthetic fixtures.

Exit 0 = fresh and HEAD-matched. Exit 1 = stale, wrong-SHA, or exe missing.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parents[1]  # cortex-speech-app/
EXE_PATH = APP_ROOT / "src-tauri" / "target" / "release" / "cortex-speech-app.exe"

# Source surfaces whose change must invalidate a stale exe. Frontend (src/**) matters because a
# bare `cargo build --release` ships a STALE embedded UI; backend (src-tauri/src/**) and the build
# inputs matter for the compiled binary itself.
SOURCE_DIRS = ["src", "src-tauri/src"]
SOURCE_FILES = [
    "src-tauri/build.rs",
    "src-tauri/tauri.conf.json",
    "src-tauri/Cargo.toml",
    "package.json",
    "index.html",
]

_SHA_MARKER = re.compile(rb"CORTEX_BUILD_SHA:([0-9a-fA-F]{7,40}|unknown)")


def extract_baked_sha(exe_bytes: bytes) -> str | None:
    """Recover the SHA baked into the binary, or None if the marker is absent."""
    match = _SHA_MARKER.search(exe_bytes)
    if match is None:
        return None
    return match.group(1).decode("ascii")


def newest_source(app_root: Path, source_dirs: list[str], source_files: list[str]) -> tuple[float, Path | None]:
    """Return (newest mtime, file) across all tracked source surfaces."""
    newest_mtime = 0.0
    newest_file: Path | None = None
    candidates: list[Path] = []
    for rel in source_dirs:
        base = app_root / rel
        if base.exists():
            candidates.extend(p for p in base.rglob("*") if p.is_file())
    for rel in source_files:
        p = app_root / rel
        if p.is_file():
            candidates.append(p)
    for p in candidates:
        mtime = p.stat().st_mtime
        if mtime > newest_mtime:
            newest_mtime = mtime
            newest_file = p
    return newest_mtime, newest_file


def evaluate_freshness(
    *,
    exe_exists: bool,
    exe_mtime: float,
    baked_sha: str | None,
    head_sha: str | None,
    newest_src_mtime: float,
    newest_src_file: str | None,
) -> list[str]:
    """Pure decision core. Returns a list of problems; empty list means fresh + HEAD-matched."""
    problems: list[str] = []

    if not exe_exists:
        problems.append("release exe not found — run `make build-app` (or `npm run tauri build`) first")
        return problems

    if newest_src_mtime > exe_mtime:
        problems.append(
            f"STALE EXE: source {newest_src_file} (mtime {newest_src_mtime:.0f}) is newer than the "
            f"built exe (mtime {exe_mtime:.0f}). Rebuild with `make build-app`."
        )

    if baked_sha is None:
        problems.append(
            "could not recover CORTEX_BUILD_SHA marker from the exe — the binary predates the P0.2 "
            "marker (lib.rs GIT_SHA_MARKER); rebuild with `make build-app`."
        )
    elif baked_sha == "unknown":
        problems.append("exe was built outside a git checkout (baked SHA = 'unknown'); rebuild in-repo.")
    elif head_sha is None:
        problems.append("could not resolve git HEAD to compare against the baked SHA.")
    elif not head_sha.startswith(baked_sha) and not baked_sha.startswith(head_sha):
        problems.append(
            f"EXE IS NOT HEAD: baked SHA {baked_sha} != git HEAD {head_sha}. "
            f"Commit, then rebuild with `make build-app` so the shipped exe matches HEAD."
        )

    return problems


def _git_head(app_root: Path) -> str | None:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=app_root,
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def main() -> int:
    exe_exists = EXE_PATH.is_file()
    exe_mtime = EXE_PATH.stat().st_mtime if exe_exists else 0.0
    baked_sha = extract_baked_sha(EXE_PATH.read_bytes()) if exe_exists else None
    head_sha = _git_head(APP_ROOT)
    newest_src_mtime, newest_src_file = newest_source(APP_ROOT, SOURCE_DIRS, SOURCE_FILES)

    problems = evaluate_freshness(
        exe_exists=exe_exists,
        exe_mtime=exe_mtime,
        baked_sha=baked_sha,
        head_sha=head_sha,
        newest_src_mtime=newest_src_mtime,
        newest_src_file=str(newest_src_file.relative_to(APP_ROOT)) if newest_src_file else None,
    )

    if problems:
        print("EXE FRESHNESS GATE: FAIL", flush=True)
        for p in problems:
            print(f"  - {p}", flush=True)
        return 1

    print(f"EXE FRESHNESS GATE: OK (exe at HEAD {head_sha[:12]}…, newer than all sources)", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
