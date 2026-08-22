#!/usr/bin/env python3
"""P0.2 — the stale-exe guard (deep-audit F4).

Proves, WITHOUT running the app, that the built release exe (a) is newer than every source file
that feeds it, (b) carries the current git HEAD, and (c) has no uncommitted compiled inputs that
the HEAD marker cannot identify. The SHA is recovered from a
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
REPO_ROOT = APP_ROOT.parent
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
SOURCE_DIRS = [
    "cortex-speech-app/src",
    "cortex-speech-app/src-tauri/src",
    "cortex-speech-app/src-tauri/assets",
    "cortex-speech-app/src-tauri/capabilities",
    "cortex-speech-app/src-tauri/migrations",
    # This path dependency is compiled into the executable. It also owns the LAN review server's
    # request/response deadlines, so omitting it could certify an exe built from older security code.
    "cortex-speech-app/src-tauri/vendor/tiny_http_fork/src",
]
SOURCE_FILES = [
    # Build configuration is source: each of these changes the SHIPPED artifact with no .rs/.svelte
    # edit (dependency pins, compiler options, bundler config), and the hunt found commits touching
    # only these being classified "non-source" — so a stale exe read as current at HEAD.
    "cortex-speech-app/package-lock.json",
    "cortex-speech-app/svelte.config.js",
    "cortex-speech-app/vite.config.ts",
    "cortex-speech-app/tsconfig.json",
    "cortex-speech-app/src-tauri/build.rs",
    "cortex-speech-app/src-tauri/tauri.conf.json",
    "cortex-speech-app/src-tauri/Cargo.toml",
    "cortex-speech-app/src-tauri/vendor/tiny_http_fork/Cargo.toml",
    "cortex-speech-app/package.json",
    "cortex-speech-app/index.html",
    # review_pilot.rs embeds this exact 8,274-ID contract with include_str!; it is runtime code even
    # though it lives at the app root rather than beneath src-tauri/src.
    "cortex-speech-app/controlled_pilot_focus.json",
    # Tauri's Windows config packages these exact runtime resources. The model binaries are
    # hash-pinned by fetch_models --check; listing them here additionally prevents an installer made
    # before a verified resource/client update from being certified as current.
    "cortex-speech-app/scripts/cortex_7b_server.py",
    "cortex-speech-app/scripts/cortex_7b_client.py",
    "cortex-speech-app/src-tauri/models/silero_vad_v4.onnx",
    "cortex-speech-app/src-tauri/models/onnxruntime.dll/onnxruntime.dll",
    "cortex-speech-app/src-tauri/models/onnxruntime.dll/onnxruntime_providers_shared.dll",
    # Same class as the assets above: each of these changes the BINARY without any .rs edit.
    # Cargo.lock pins the exact dependency versions compiled in, so `cargo update` alone rebuilds a
    # different exe while Cargo.toml is untouched. capabilities/default.json is the Tauri v2 ACL that
    # tauri-build compiles into the app — a permission change is a behaviour change. This IS the
    # Windows build, so tauri.windows.conf.json is as load-bearing as tauri.conf.json beside it.
    # .cargo/config.toml carries build flags that change codegen, and icon.ico is linked into the
    # binary's resources.
    "cortex-speech-app/src-tauri/Cargo.lock",
    "cortex-speech-app/src-tauri/tauri.windows.conf.json",
    "cortex-speech-app/src-tauri/.cargo/config.toml",
    "cortex-speech-app/src-tauri/icons/icon.png",
    "cortex-speech-app/src-tauri/icons/icon.ico",
    # The pinned compiler is part of the shipped binary's reproducibility contract and lives one
    # level above APP_ROOT. Keeping every inventory entry repo-relative makes both mtime and git-diff
    # checks cover it instead of silently treating a toolchain-only commit as documentation.
    "rust-toolchain.toml",
]

_SHA_MARKER = re.compile(rb"CORTEX_BUILD_SHA:([0-9a-fA-F]{7,40}|unknown)")


def extract_baked_sha(exe_bytes: bytes) -> str | None:
    """Recover the SHA baked into the binary, or None if the marker is absent."""
    match = _SHA_MARKER.search(exe_bytes)
    if match is None:
        return None
    return match.group(1).decode("ascii")


def newest_source(source_root: Path, source_dirs: list[str], source_files: list[str]) -> tuple[float, Path | None]:
    """Return (newest mtime, file) across all tracked source surfaces."""
    newest_mtime = 0.0
    newest_file: Path | None = None
    candidates: list[Path] = []
    for rel in source_dirs:
        base = source_root / rel
        if base.exists():
            candidates.extend(p for p in base.rglob("*") if p.is_file())
    for rel in source_files:
        p = source_root / rel
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
    dirty_source_paths: list[str] | None = None,
    source_status_available: bool = True,
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
            f"STALE INSTALLER: {name} (mtime {mtime:.0f}) predates this checkout's newest source "
            f"(mtime {newest_src_mtime:.0f}) — it would install an app built from older code. "
            f"Rebuild with `npm run tauri build`, or delete it so nothing can install an old version."
        )

    # A commit marker cannot identify uncommitted inputs. Timestamps can show that the exe is newer
    # than the files currently on disk, but they cannot prove which dirty bytes the compiler read.
    # Refuse the production claim until every compiled input belongs to an immutable commit.
    if not source_status_available:
        problems.append("could not inspect the current worktree for uncommitted compiled-source inputs")
    elif dirty_source_paths:
        shown = ", ".join(dirty_source_paths[:5])
        remainder = len(dirty_source_paths) - 5
        if remainder > 0:
            shown += f", and {remainder} more"
        problems.append(
            "UNCOMMITTED COMPILED SOURCE: the exe cannot be certified as reproducibly at HEAD while "
            f"these build inputs are dirty: {shown}. Commit/version the source, then rebuild."
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
    *(f"{directory.rstrip('/')}/" for directory in SOURCE_DIRS),
    *SOURCE_FILES,
]


def worktree_source_changes(status_lines: list[str], source_prefixes: list[str]) -> list[str]:
    """Compiled/build inputs named by porcelain status, including untracked files and renames."""
    changes: set[str] = set()
    for line in status_lines:
        if not line.strip():
            continue
        payload = line[3:] if len(line) >= 3 else line.strip()
        for raw_path in payload.split(" -> "):
            path = raw_path.strip().strip('"').replace("\\", "/")
            if any(
                path.startswith(prefix) if prefix.endswith("/") else path == prefix
                for prefix in source_prefixes
            ):
                changes.add(path)
    return sorted(changes)


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
        dirty = worktree_source_changes(status_lines, source_prefixes)
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


def _git_status(app_root: Path) -> list[str] | None:
    """Current checkout porcelain, or None when git cannot provide a trustworthy answer."""
    try:
        out = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=app_root,
            capture_output=True,
            text=True,
            check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    return out.stdout.splitlines()


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


def find_stale_installers(newest_src_mtime: float) -> list[tuple[str, float]]:
    """Bundled installers that would install an app built from OLDER SOURCES than this checkout.

    Measured against the newest SOURCE, not against the exe. Comparing installer-vs-exe asks the
    wrong question: one `tauri build` writes the MSI, then patches the exe for NSIS, then writes the
    setup exe, so a single build's own artifacts straddle its exe mtime (measured 2026-08-20: the MSI
    of the very build being verified sat 336 s "behind" the exe it embeds). Answering that with a
    time tolerance meant picking a window, and any window wide enough for a build (~6 min here) is
    also wide enough to pass an installer from the PREVIOUS build — a gate with a build-sized hole.

    The question that actually matters is the same one asked of the exe: does this artifact predate
    the sources it claims to ship? That has an exact answer and needs no fudge factor. The incident
    this gate exists for (2026-08-17, an installer four DAYS behind) is caught either way; a
    same-build artifact 336 s "behind" its exe is still newer than every source and passes honestly.
    """
    stale: list[tuple[str, float]] = []
    for pattern in INSTALLER_GLOBS:
        for path in sorted(BUNDLE_DIR.glob(pattern)):
            mtime = path.stat().st_mtime
            if mtime < newest_src_mtime:
                stale.append((path.name, mtime))
    return stale


def main() -> int:
    exe_exists = EXE_PATH.is_file()
    exe_mtime = EXE_PATH.stat().st_mtime if exe_exists else 0.0
    baked_sha = extract_baked_sha(EXE_PATH.read_bytes()) if exe_exists else None
    head_sha = _git_head(REPO_ROOT)

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
        changed = _source_changed_since(REPO_ROOT, baked_sha, SOURCE_DIRS, SOURCE_FILES)
        if changed is not None and len(changed) == 0:
            effective_head = baked_sha
            note = f"HEAD advanced to {head_sha[:12]}… via non-source commits; no source changed since the build."

    newest_src_mtime, newest_src_file = newest_source(REPO_ROOT, SOURCE_DIRS, SOURCE_FILES)
    current_status = _git_status(REPO_ROOT)
    dirty_sources = worktree_source_changes(current_status or [], SOURCE_PREFIXES)

    problems = evaluate_freshness(
        exe_exists=exe_exists,
        exe_mtime=exe_mtime,
        baked_sha=baked_sha,
        head_sha=effective_head,
        newest_src_mtime=newest_src_mtime,
        newest_src_file=str(newest_src_file.relative_to(REPO_ROOT)) if newest_src_file else None,
        stale_installers=find_stale_installers(newest_src_mtime) if exe_exists else [],
        dirty_source_paths=dirty_sources,
        source_status_available=current_status is not None,
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
    wt_warnings = worktree_source_warnings(_git_worktrees(REPO_ROOT), str(REPO_ROOT), SOURCE_PREFIXES)
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
