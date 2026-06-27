import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
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
    ]
    # This file defines the forbidden patterns as string literals and documents them in comments, so it
    # would match itself — a pattern-detector cannot scan its own pattern definitions. Exempt it.
    self_path = Path(__file__).resolve()
    for path in iter_repo_paths(REPO_ROOT):
        if not path.is_file() or path.suffix.lower() not in TEXT_EXTENSIONS:
            continue
        if path.resolve() == self_path:
            continue
        text = path.read_text(encoding="utf-8", errors="ignore")
        for line_no, line in enumerate(text.splitlines(), start=1):
            # Collapse runs of back-slashes to one so a JSON-escaped path (C:\\Users\\, even C:\\\\Users)
            # normalizes to C:\Users\ and is caught like the plain native form.
            normalized = re.sub(r"\\+", "\\\\", line)
            if any(forbidden in normalized for forbidden in forbidden_paths):
                offenders.append(f"{path.relative_to(REPO_ROOT)}:{line_no}:{line.strip()}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(f"Source files must not hardcode private local Windows paths:\n{formatted}")


def main() -> None:
    test_no_windows_reserved_repo_entries()
    test_no_hardcoded_local_windows_profile_paths()
    print("windows repo hygiene regression passed")


if __name__ == "__main__":
    main()
