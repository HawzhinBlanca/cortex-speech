import json
import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER = REPO_ROOT / "scripts" / "test-real-data.ps1"


def runner_text() -> str:
    return RUNNER.read_text(encoding="utf-8")


def package_json() -> dict:
    return json.loads((REPO_ROOT / "package.json").read_text(encoding="utf-8"))


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def test_mp4_temp_cleanup_is_constrained_to_temp() -> None:
    text = runner_text()

    assert_contains(text, "function Remove-TemporaryFixtureDir", RUNNER.name)
    assert_contains(text, "Resolve-Path -LiteralPath $Path -ErrorAction Stop", RUNNER.name)
    assert_contains(text, "Resolve-Path -LiteralPath $env:TEMP -ErrorAction Stop", RUNNER.name)
    assert_contains(text, "$normalizedPath.Equals($normalizedTemp", RUNNER.name)
    assert_contains(text, "$normalizedPath.StartsWith($tempPrefix", RUNNER.name)
    assert_contains(text, "Refusing to remove temporary fixture directory outside TEMP", RUNNER.name)
    if "GetRelativePath" in text:
        raise AssertionError("test-real-data.ps1 must stay compatible with Windows PowerShell 5.1")


def test_recursive_remove_only_exists_inside_safety_helper() -> None:
    text = runner_text()
    remove_lines = [
        line.strip()
        for line in text.splitlines()
        if re.search(r"\bRemove-Item\b", line) and "-Recurse" in line
    ]

    expected = ["Remove-Item -LiteralPath $resolvedPath -Recurse -Force"]
    if remove_lines != expected:
        raise AssertionError(
            "Recursive Remove-Item must only appear inside Remove-TemporaryFixtureDir. "
            f"Found: {remove_lines}"
        )

    if text.count("Remove-TemporaryFixtureDir -Path $fixtureDir") != 1:
        raise AssertionError("Mp4Only setup must clean stale fixture dirs through Remove-TemporaryFixtureDir")
    if text.count("Remove-TemporaryFixtureDir -Path $mp4FixtureDir") != 1:
        raise AssertionError("Mp4Only finally cleanup must use Remove-TemporaryFixtureDir")


def test_package_real_audio_scripts_use_the_checked_runner() -> None:
    scripts = package_json()["scripts"]
    expected_prefix = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-real-data.ps1"

    for name in [
        "test:real-audio",
        "test:real-audio:user",
        "test:real-audio:integration",
        "test:real-audio:integration:mp4",
    ]:
        command = scripts.get(name, "")
        if not command.startswith(expected_prefix):
            raise AssertionError(f"package.json {name} must invoke scripts/test-real-data.ps1 safely")


def main() -> None:
    test_mp4_temp_cleanup_is_constrained_to_temp()
    test_recursive_remove_only_exists_inside_safety_helper()
    test_package_real_audio_scripts_use_the_checked_runner()
    print("real-data runner policy regression passed")


if __name__ == "__main__":
    main()
