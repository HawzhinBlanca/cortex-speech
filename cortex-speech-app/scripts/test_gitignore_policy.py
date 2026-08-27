import re
from pathlib import Path

from _pipeline_policy_util import pipeline_surface


REPO_ROOT = Path(__file__).resolve().parents[1]
GITIGNORE = REPO_ROOT / ".gitignore"

SUPPORTED_PRIVATE_MEDIA_EXTENSIONS = [
    "wav",
    "mp3",
    "flac",
    "m4a",
    "ogg",
    "aac",
    "opus",
    "mp4",
    "mov",
    "webm",
    "wma",
]

REQUIRED_STATIC_IGNORE_RULES = [
    "node_modules/",
    "dist/",
    "target/",
    "src-tauri/target/",
    "playwright-report/",
    "test-results/",
    "coverage/",
    "reports/",
    "logs/",
    "*.db",
    "*.db-journal",
    "*.db-wal",
    "*.db-shm",
    ".env",
    ".env.local",
    "*.log",
    "*.tmp",
    "*.temp",
    "*.tar",
    "*.tar.gz",
    "*.tar.bz2",
    "*.zip",
    "*.onnx",
    "src-tauri/models/",
    "!src-tauri/models/.gitkeep",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
]


def read(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def gitignore_rules() -> set[str]:
    return {
        line.strip()
        for line in GITIGNORE.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.strip().startswith("#")
    }


def bracketed_extensions(text: str, marker: str) -> set[str]:
    marker_index = text.find(marker)
    if marker_index < 0:
        raise AssertionError(f"Could not find extension marker: {marker}")
    if "add_filter" in marker:
        value_start = text.find("&[", marker_index)
    else:
        equals = text.find("=", marker_index)
        value_start = text.find("[", equals if equals >= 0 else marker_index) - 1
    if value_start >= 0 and text[value_start : value_start + 2] == "&[":
        start = value_start + 1
    else:
        start = value_start + 1 if value_start >= 0 else -1
    end = text.find("]", start)
    if start < 0 or end < 0:
        raise AssertionError(f"Could not find bracketed extension list after: {marker}")
    return {value.lstrip(".") for value in re.findall(r'"(\.?[a-z0-9]+)"', text[start:end])}


def powershell_supported_extensions() -> set[str]:
    text = read("scripts/test-real-data.ps1")
    match = re.search(r"\$SupportedAudioExtensions\s*=\s*@\(([^)]*)\)", text)
    if not match:
        raise AssertionError("scripts/test-real-data.ps1 is missing $SupportedAudioExtensions")
    return {value.lstrip(".") for value in re.findall(r'"(\.?[a-z0-9]+)"', match.group(1))}


def readme_supported_extensions() -> set[str]:
    text = read("README.md")
    for line in text.splitlines():
        values = re.findall(r"`([a-z0-9]+)`", line)
        if "wav" in values and "mp3" in values:
            return set(values)
    raise AssertionError("README.md is missing the supported audio format list")


def test_supported_private_media_formats_are_consistent() -> None:
    expected = set(SUPPORTED_PRIVATE_MEDIA_EXTENSIONS)
    actual_sets = {
        "README.md": readme_supported_extensions(),
        "scripts/test-real-data.ps1": powershell_supported_extensions(),
        "src-tauri/src/commands.rs": bracketed_extensions(read("src-tauri/src/commands.rs"), 'add_filter("Audio"'),
        "src-tauri/src/pipeline surface": bracketed_extensions(
            pipeline_surface(REPO_ROOT / "src-tauri" / "src"), "let audio_exts ="
        ),
        "src-tauri/tests/real_audio.rs": bracketed_extensions(read("src-tauri/tests/real_audio.rs"), "SUPPORTED_AUDIO_EXTENSIONS"),
    }
    mismatches = {
        name: sorted(values)
        for name, values in actual_sets.items()
        if values != expected
    }
    if mismatches:
        expected_list = ", ".join(SUPPORTED_PRIVATE_MEDIA_EXTENSIONS)
        details = "\n".join(f"- {name}: {values}" for name, values in mismatches.items())
        raise AssertionError(f"Supported media format lists must match [{expected_list}]:\n{details}")


def test_generated_and_private_artifacts_are_ignored() -> None:
    rules = gitignore_rules()
    required = REQUIRED_STATIC_IGNORE_RULES + [f"*.{extension}" for extension in SUPPORTED_PRIVATE_MEDIA_EXTENSIONS]
    missing = [rule for rule in required if rule not in rules]
    if missing:
        formatted = "\n".join(f"- {rule}" for rule in missing)
        raise AssertionError(f".gitignore is missing required release-hygiene rules:\n{formatted}")


def main() -> None:
    test_supported_private_media_formats_are_consistent()
    test_generated_and_private_artifacts_are_ignored()
    print("gitignore policy regression passed")


if __name__ == "__main__":
    main()
