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
#
# src-tauri/assets and src-tauri/migrations are here because they are COMPILED IN, not read from
# disk at runtime: `include_str!("../assets/couch.html")`, `include_bytes!("../assets/couch-icon.png")`
# and `include_str!("../../migrations/001_initial.sql")`. Editing the phone review page — 68 KB of
# reviewer-facing behaviour, its Sorani strings included — therefore stales the exe exactly as a .rs
# edit does, and this gate could not see it. Caught live: couch.html sat 15 minutes newer than the
# binary while the gate printed "newer than all sources".
SOURCE_DIRS = ["src", "src-tauri/src", "src-tauri/assets", "src-tauri/migrations"]
SOURCE_FILES = [
    # Build configuration is source: each of these changes the SHIPPED artifact with no .rs/.svelte
    # edit (dependency pins, compiler options, bundler config), and the hunt found commits touching
    # only these being classified "non-source" — so a stale exe read as current at HEAD.
    "package-lock.json",
    "svelte.config.js",
    "vite.config.ts",
    "tsconfig.json",
    "src-tauri/build.rs",
    "src-tauri/tauri.conf.json",
    "src-tauri/Cargo.toml",
    "package.json",
    "index.html",
    # Same class as the assets above: each of these changes the BINARY without any .rs edit.
    # Cargo.lock pins the exact dependency versions compiled in, so `cargo update` alone rebuilds a
    # different exe while Cargo.toml is untouched. capabilities/default.json is the Tauri v2 ACL that
    # tauri-build compiles into the app — a permission change is a behaviour change. This IS the
    # Windows build, so tauri.windows.conf.json is as load-bearing as tauri.conf.json beside it.
    # .cargo/config.toml carries build flags that change codegen, and icon.ico is linked into the
    # binary's resources.
    "src-tauri/Cargo.lock",
    "src-tauri/capabilities/default.json",
    "src-tauri/tauri.windows.conf.json",
    "src-tauri/.cargo/config.toml",
    "src-tauri/icons/icon.ico",
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
    stale_installers: list[tuple[str, float]] | None = None,
) -> list[str]:
    """Pure decision core. Returns a list of problems; empty list means fresh + HEAD-matched."""
    problems: list[str] = []

    # Bundled installers older than the exe are the same lie one directory over: an MSI/NSIS built
    # from a previous commit still sits in `target/release/bundle/` looking finished, and it is the
    # artifact anyone would actually double-click. Found 2026-08-17: both installers were four days
    # behind the exe. Reported as a problem, not silently deleted — half a gigabyte each is the
    # owner's to keep or discard.
    for name, mtime in stale_installers or []:
        problems.append(
            f"STALE INSTALLER: {name} (mtime {mtime:.0f}) predates the built exe (mtime {exe_mtime:.0f}). "
            f"Rebuild with `npm run tauri build`, or delete it so nothing can install an old version."
        )

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


SOURCE_PREFIXES = [
    "cortex-speech-app/src/",
    "cortex-speech-app/src-tauri/src",
    "cortex-speech-app/src-tauri/build.rs",
    "cortex-speech-app/src-tauri/Cargo.toml",
    "cortex-speech-app/src-tauri/tauri.conf.json",
    "cortex-speech-app/package.json",
    "cortex-speech-app/index.html",
]


def worktree_source_warnings(
    worktrees: list[tuple[str, list[str]]], current_root: str, source_prefixes: list[str]
) -> list[str]:
    """Pure core (unit-tested): given [(worktree_path, porcelain_status_lines)] and the git root of the
    checkout being gated, warn for OTHER worktrees carrying UNCOMMITTED changes under a source surface.

    A green freshness gate means only "the built exe matches THIS checkout's HEAD". It must not hide the
    fact that a sibling worktree has unshipped source edits (the exact stale-exe-vs-worktree scenario this
    session hit). Non-fatal — WIP on a branch is legitimate; the point is to make it VISIBLE.
    """
    warnings: list[str] = []
    current = str(Path(current_root).resolve())
    for path, status_lines in worktrees:
        if str(Path(path).resolve()) == current:
            continue  # the checkout being gated
        dirty = [ln for ln in status_lines if ln.strip() and any(pfx in ln for pfx in source_prefixes)]
        if dirty:
            warnings.append(
                f"sibling worktree {path} has {len(dirty)} uncommitted source change(s) not reflected in the built exe"
            )
    return warnings


def _git_worktrees(app_root: Path) -> list[tuple[str, list[str]]]:
    """[(worktree_root, porcelain_status_lines)] for every registered worktree; [] if git is unavailable."""
    try:
        wt_out = subprocess.run(
            ["git", "worktree", "list", "--porcelain"], cwd=app_root, capture_output=True, text=True, check=True
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []
    paths = [ln[len("worktree ") :].strip() for ln in wt_out.stdout.splitlines() if ln.startswith("worktree ")]
    result: list[tuple[str, list[str]]] = []
    for p in paths:
        try:
            st = subprocess.run(["git", "-C", p, "status", "--porcelain"], capture_output=True, text=True, check=True)
            result.append((p, st.stdout.splitlines()))
        except (subprocess.CalledProcessError, FileNotFoundError):
            continue
    return result


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


def _source_changed_since(app_root: Path, baked_sha: str, source_dirs: list[str], source_files: list[str]) -> list[str] | None:
    """Source-relative paths changed between the baked commit and HEAD, or None if git can't tell.

    The SHA-equality check is only a proxy for "the exe reflects the current source." When HEAD
    advances for non-source reasons (docs, ledger), the exe is still fresh. This narrows the SHA
    check to what actually matters: did any SOURCE file change since the exe was built?
    """
    paths = [*source_dirs, *source_files]
    try:
        out = subprocess.run(
            ["git", "diff", "--name-only", baked_sha, "HEAD", "--", *paths],
            cwd=app_root,
            capture_output=True,
            text=True,
            check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    return [line for line in out.stdout.splitlines() if line.strip()]


BUNDLE_DIR = EXE_PATH.parent / "bundle"
INSTALLER_GLOBS = ("msi/*.msi", "nsis/*-setup.exe")


def find_stale_installers(exe_mtime: float) -> list[tuple[str, float]]:
    """Bundled installers that predate the exe — i.e. would install an older app than this build."""
    stale: list[tuple[str, float]] = []
    for pattern in INSTALLER_GLOBS:
        for path in sorted(BUNDLE_DIR.glob(pattern)):
            mtime = path.stat().st_mtime
            # 15-minute tolerance: ONE `tauri build` writes the MSI, then patches the exe for
            # NSIS, then writes the setup exe — so the artifacts of a single build straddle the
            # exe's final mtime by however long candle/light take (measured 2026-08-20: the MSI of
            # the very build being verified sat 336s "behind" the exe it embeds). The incident this
            # gate exists for (2026-08-17) was four DAYS behind, far outside any bundling window.
            if mtime < exe_mtime - 900.0:
                stale.append((path.name, mtime))
    return stale


def main() -> int:
    exe_exists = EXE_PATH.is_file()
    exe_mtime = EXE_PATH.stat().st_mtime if exe_exists else 0.0
    baked_sha = extract_baked_sha(EXE_PATH.read_bytes()) if exe_exists else None
    head_sha = _git_head(APP_ROOT)

    # Narrow the SHA-equality check to what matters: if HEAD advanced past the baked commit but no
    # SOURCE file changed (e.g. a docs/ledger commit), the exe still reflects the source — treat the
    # baked commit as HEAD-equivalent for the gate and say so.
    effective_head = head_sha
    note = None
    if (
        exe_exists
        and baked_sha not in (None, "unknown")
        and head_sha is not None
        and not (head_sha.startswith(baked_sha) or baked_sha.startswith(head_sha))
    ):
        changed = _source_changed_since(APP_ROOT, baked_sha, SOURCE_DIRS, SOURCE_FILES)
        if changed is not None and len(changed) == 0:
            effective_head = baked_sha
            note = f"HEAD advanced to {head_sha[:12]}… via non-source commits; no source changed since the build."

    newest_src_mtime, newest_src_file = newest_source(APP_ROOT, SOURCE_DIRS, SOURCE_FILES)

    problems = evaluate_freshness(
        exe_exists=exe_exists,
        exe_mtime=exe_mtime,
        baked_sha=baked_sha,
        head_sha=effective_head,
        newest_src_mtime=newest_src_mtime,
        newest_src_file=str(newest_src_file.relative_to(APP_ROOT)) if newest_src_file else None,
        stale_installers=find_stale_installers(exe_mtime) if exe_exists else [],
    )
    if note and not problems:
        print(f"note: {note}", flush=True)

    if problems:
        print("EXE FRESHNESS GATE: FAIL", flush=True)
        for p in problems:
            print(f"  - {p}", flush=True)
        return 1

    # Green means "the exe matches THIS checkout's HEAD" — but a sibling worktree may hold unshipped
    # source edits (the stale-exe-vs-worktree trap). Surface them loudly; non-fatal (WIP is legitimate).
    wt_warnings = worktree_source_warnings(_git_worktrees(APP_ROOT), str(APP_ROOT.parent), SOURCE_PREFIXES)
    for w in wt_warnings:
        print(f"  ! WARNING: {w}", flush=True)

    print(f"EXE FRESHNESS GATE: OK (exe at HEAD {head_sha[:12]}…, newer than all sources)", flush=True)
    if wt_warnings:
        print(
            f"  (note: {len(wt_warnings)} sibling worktree(s) carry uncommitted source — commit + rebuild before shipping)",
            flush=True,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
