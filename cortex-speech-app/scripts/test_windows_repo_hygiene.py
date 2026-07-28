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


def test_no_private_network_addresses_or_device_names() -> None:
    """The owner's tailnet/LAN addresses and device names must not enter a public repo.

    Not a credential — a CGNAT 100.64/10 address is unroutable from the internet and a 192.168.x one
    is meaningless off the LAN — but it is device-identifying metadata, and it reached the ledger
    simply by pasting real evidence out of a live remote-review session. That is exactly the honest
    habit this project wants, so the fix is a gate rather than a rule nobody remembers: paste the
    evidence, keep the addresses out of it.

    Documentation of the RANGE is fine and necessary (couch.rs explains the CGNAT check); what is
    forbidden is a specific host address or hostname.
    """
    offenders: list[str] = []
    forbidden = [
        # Tailscale CGNAT hosts: 100.64.0.0/10.
        (re.compile(r"\b100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3}\b"), "tailnet host address"),
        # RFC1918 LAN hosts. Deliberately NOT scanning 10.0.0.0/8: it is indistinguishable from a
        # four-part version string (10.0.19041.1), and the owner's LAN is 192.168.x — a rule that
        # cries wolf on every version number would be turned off within a week.
        (re.compile(r"\b192\.168\.\d{1,3}\.\d{1,3}\b"), "LAN host address"),
        (re.compile(r"\bhawapc01\b|\bcomms-pc01\b|\bdesktop-pkdj819\b|\bmacbook-pro-3\b", re.I), "device hostname"),
        (re.compile(r"\biphone-15-pro-max\b", re.I), "device hostname"),
    ]
    # Addresses that are NOT anyone's device, listed individually rather than by exempting whole files
    # — a path exemption is a hole a real address can later be pasted into, and these are fixtures that
    # should stay frozen anyway.
    allowed_literals = {
        "100.100.100.100",  # Tailscale's public MagicDNS anycast address; couch.rs routes to it by design
        "100.64.0.2",  # synthetic reviewer URL in e2e/helpers/tauri-mock.ts
        "192.168.0.2",  # ditto
    }
    self_rel = str(Path(__file__).resolve().relative_to(GIT_ROOT.resolve())).replace("\\", "/")
    try:
        tracked = _git_tracked_text_files(GIT_ROOT)
    except (subprocess.CalledProcessError, FileNotFoundError):
        return  # no git: the private-path test already covers the filesystem walk
    for rel, path in tracked:
        if rel == self_rel:
            continue  # this file defines the patterns and would match itself
        for line_no, line in enumerate(path.read_text(encoding="utf-8", errors="ignore").splitlines(), start=1):
            for pattern, what in forbidden:
                for match in pattern.finditer(line):
                    hit = match.group(0)
                    if hit in allowed_literals:
                        continue
                    # CIDR notation names a RANGE, not a host: `100.64.0.0/10` is the documentation the
                    # CGNAT check in couch.rs is built on and must stay readable.
                    if line[match.end() : match.end() + 1] == "/":
                        continue
                    offenders.append(f"{rel}:{line_no}: {what} {hit!r} in: {line.strip()[:90]}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(
            "Tracked files must not carry a specific private network address or device name (public "
            "repo). Redact to a placeholder like <this-pc-tailnet-ip>; the evidence is just as good "
            "without the octets:\n" + formatted
        )


def main() -> None:
    test_no_windows_reserved_repo_entries()
    test_no_hardcoded_local_windows_profile_paths()
    test_no_private_network_addresses_or_device_names()
    print("windows repo hygiene regression passed")


if __name__ == "__main__":
    main()
