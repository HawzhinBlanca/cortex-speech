import json
import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER = REPO_ROOT / "scripts" / "test-real-data.ps1"
E2E = REPO_ROOT / "e2e_real_app.cjs"
CLEAR_DB = REPO_ROOT / "clear_db.py"


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


def test_e2e_never_clears_the_real_db_by_default() -> None:
    # A reliability test must never be CAPABLE of erasing the owner's real library just by being run.
    # DB-clear is opt-in (CORTEX_DB_CLEAR=1), never the old opt-out default.
    e2e = E2E.read_text(encoding="utf-8")
    assert_contains(e2e, "process.env.CORTEX_DB_CLEAR === '1'", E2E.name)
    if "if (!SKIP_DB_CLEAR)" in e2e:
        raise AssertionError("e2e_real_app.cjs must not clear the DB by default (opt-out); make it opt-in")


def test_e2e_is_isolated_from_the_production_profile() -> None:
    # P0 isolation contract: the harness runs against a DISPOSABLE profile, refuses the real one,
    # kills only the process tree it spawned, and reads its manifest from the isolated DB.
    e2e = E2E.read_text(encoding="utf-8")
    # 1. Disposable profile by default + production-profile refusal.
    assert_contains(e2e, "mkdtempSync", E2E.name)
    assert_contains(e2e, "REFUSED: CORTEX_APP_DATA_DIR points at the REAL profile", E2E.name)
    # 2. The spawned exe is pointed at the isolated profile.
    assert_contains(e2e, "CORTEX_APP_DATA_DIR: DATA_DIR", E2E.name)
    # 3. Never kill by image name — that would kill the owner's own running app.
    if "/IM cortex-speech-app.exe" in e2e:
        raise AssertionError("e2e_real_app.cjs must not taskkill by image name; kill only the spawned PID tree")
    assert_contains(e2e, "taskkill /F /T /PID", E2E.name)
    # 4. The run manifest must come from the isolated DB, never the production %APPDATA% path.
    if "%APPDATA%" in e2e and "cortex-speech.db" in e2e.split("%APPDATA%")[1][:80]:
        raise AssertionError("e2e_real_app.cjs must not read the production %APPDATA% database")


def test_clear_db_snapshots_before_deleting_and_requires_confirmation() -> None:
    clr = CLEAR_DB.read_text(encoding="utf-8")
    # Refuses without explicit confirmation.
    assert_contains(clr, "CORTEX_DB_CLEAR_CONFIRM", CLEAR_DB.name)
    assert_contains(clr, "REFUSING to clear", CLEAR_DB.name)
    # Snapshot MUST happen before any DELETE — a clear is always recoverable.
    snap_at = clr.find("shutil.copy2")
    del_at = clr.find("DELETE FROM")
    if snap_at < 0 or del_at < 0 or snap_at > del_at:
        raise AssertionError("clear_db.py must snapshot the DB (shutil.copy2) BEFORE any DELETE FROM")


def main() -> None:
    test_mp4_temp_cleanup_is_constrained_to_temp()
    test_recursive_remove_only_exists_inside_safety_helper()
    test_package_real_audio_scripts_use_the_checked_runner()
    test_e2e_never_clears_the_real_db_by_default()
    test_e2e_is_isolated_from_the_production_profile()
    test_clear_db_snapshots_before_deleting_and_requires_confirmation()
    print("real-data runner policy regression passed")


if __name__ == "__main__":
    main()
