import re
import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
# The actual git root / PUBLIC remote is the PARENT of cortex-speech-app/ (which is a plain subdirectory,
# not a submodule). The private-path gate must scan that whole public surface, or root-level dev scripts
# leak the owner's profile path with the gate still green.
GIT_ROOT = REPO_ROOT.parent
# NO exemptions. The private per-owner datasets that used to be exempted here are now gitignored and
# untracked (F3, 2026-07-02), so the path-hygiene scan holds the entire tracked surface with zero holes.
SKIP_DIRS = {
    ".claude",  # per-machine editor/tool config (settings.local.json), not shipped app source
    ".git",
    ".svelte-kit",
    "dist",
    "node_modules",
    "playwright-report",
    "target",
    "target-health",
    "test-results",
}
TEXT_EXTENSIONS = {
    ".css",
    ".html",
    ".js",
    ".json",
    ".md",
    ".ps1",
    ".py",
    ".rs",
    ".svelte",
    ".toml",
    ".ts",
    ".tsx",
    ".yml",
    ".yaml",
}
WINDOWS_RESERVED_NAMES = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{i}" for i in range(1, 10)),
    *(f"LPT{i}" for i in range(1, 10)),
}


def is_windows_reserved_name(path: Path) -> bool:
    base = path.name.rstrip(" .")
    stem = base.split(".", 1)[0].upper()
    return stem in WINDOWS_RESERVED_NAMES


def iter_repo_paths(root: Path) -> list[Path]:
    paths: list[Path] = []
    for child in sorted(root.iterdir(), key=lambda p: p.name.lower()):
        if child.is_dir() and child.name in SKIP_DIRS:
            continue
        paths.append(child)
        if child.is_dir():
            paths.extend(iter_repo_paths(child))
    return paths


def test_no_windows_reserved_repo_entries() -> None:
    offenders = [path.relative_to(REPO_ROOT) for path in iter_repo_paths(REPO_ROOT) if is_windows_reserved_name(path)]
    if offenders:
        formatted = "\n".join(f"- {path}" for path in offenders)
        raise AssertionError(f"Windows-reserved repo entries break tooling:\n{formatted}")


def _git_tracked_text_files(root: Path) -> list[tuple[str, Path]]:
    """The PUBLIC surface = git-tracked text files at the git root (what actually ships). Scanning
    tracked files, not a filesystem walk, avoids false positives from untracked local scratch and
    correctly covers the whole repo, not just the cortex-speech-app/ subtree."""
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"], capture_output=True, text=True, check=True
    )
    files: list[tuple[str, Path]] = []
    for rel in out.stdout.split("\0"):
        rel = rel.strip()
        if not rel:
            continue
        path = root / rel
        if path.suffix.lower() in TEXT_EXTENSIONS and path.is_file():
            files.append((rel.replace("\\", "/"), path))
    return files


def test_no_hardcoded_local_windows_profile_paths() -> None:
    offenders: list[str] = []
    # Catch a private profile path in EVERY form a leak realistically takes, not just native
    # back-slash: source/JSON forward-slash (C:/Users/...) and the WSL mount (/mnt/c/Users/...) are
    # equally identifying and previously slipped straight through (e.g. a generated SBOM's
    # `path+file:///C:/Users/<name>/...` bom-refs). Match any user, not just one name.
    forbidden_paths = [
        "C:" + "\\Users\\",
        "C:/Users/",
        "/mnt/c/Users/",
        "D:" + "\\Hawzhin",
        "D:/Hawzhin",
        # The owner's name used as a local FOLDER (a personal-folder path fragment), in either slash
        # form. Deliberately requires a trailing separator so it catches "…/Hawzhin/…" or
        # "%CORTEX_AUDIO_LIKE%" but NOT the legitimate public GitHub handle "HawzhinBlanca".
        "Hawzhin" + "\\",
        "Hawzhin/",
    ]
    # This file defines the forbidden patterns as string literals and documents them in comments, so it
    # would match itself — a pattern-detector cannot scan its own pattern definitions. Exempt it.
    self_rel = str(Path(__file__).resolve().relative_to(GIT_ROOT.resolve())).replace("\\", "/")
    try:
        tracked = _git_tracked_text_files(GIT_ROOT)
    except (subprocess.CalledProcessError, FileNotFoundError):
        # No git available: fall back to the cortex-speech-app/ filesystem walk (original, narrower scope).
        tracked = [
            (str(p.relative_to(GIT_ROOT)).replace("\\", "/"), p)
            for p in iter_repo_paths(REPO_ROOT)
            if p.is_file() and p.suffix.lower() in TEXT_EXTENSIONS
        ]
    for rel, path in tracked:
        if rel == self_rel:
            continue
        text = path.read_text(encoding="utf-8", errors="ignore")
        for line_no, line in enumerate(text.splitlines(), start=1):
            # Collapse runs of back-slashes to one so a JSON-escaped path (C:\\Users\\, even C:\\\\Users)
            # normalizes to C:\Users\ and is caught like the plain native form.
            normalized = re.sub(r"\\+", "\\\\", line)
            if any(forbidden in normalized for forbidden in forbidden_paths):
                offenders.append(f"{rel}:{line_no}:{line.strip()}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(f"Tracked files must not hardcode a private local profile path (public repo):\n{formatted}")


def main() -> None:
    test_no_windows_reserved_repo_entries()
    test_no_hardcoded_local_windows_profile_paths()
    print("windows repo hygiene regression passed")


if __name__ == "__main__":
    main()
