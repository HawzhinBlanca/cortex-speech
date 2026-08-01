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
    # 5. The WebView2 browser profile is the SECOND shared resource and must be isolated too.
    #    Tauri keys it on the bundle identity, not on CORTEX_APP_DATA_DIR, so a run spawned while the
    #    owner's own Cortex is open shares the folder. WebView2 then refuses the environment
    #    (HRESULT 0x8007139F) and silently drops --remote-debugging-port, and this harness times out
    #    on a port nobody opened. Measured 2026-08-01: FAIL in 92.0s with the app open, PASS with the
    #    folder isolated. Without this assertion the gate's green depends on machine state.
    assert_contains(e2e, "WEBVIEW2_USER_DATA_FOLDER: WEBVIEW2_DIR", E2E.name)


def test_e2e_profile_cleanup_is_guarded_and_keeps_evidence_on_failure() -> None:
    # The harness mints a disposable profile per run and, since the WebView2 isolation, a ~11 MB browser
    # profile with it. Nothing removed them (measured: 34 dirs / 764 MB), so cleanup was added — and a
    # recursive delete in a test harness needs the same guards as Remove-TemporaryFixtureDir does in
    # scripts/test-real-data.ps1: only a directory WE created, only under the temp root, never a
    # caller-supplied one.
    e2e = E2E.read_text(encoding="utf-8")
    # Comment lines are stripped first: the rule is about executable deletes, and a naive substring
    # scan fails on the comment that EXPLAINS the delete — which is prose, not a second call site.
    code = [ln.strip() for ln in e2e.splitlines() if not ln.strip().startswith(("//", "*", "/*"))]
    rm_lines = [ln for ln in code if "rmSync" in ln]
    if rm_lines != ["fs.rmSync(target, { recursive: true, force: true });"]:
        raise AssertionError(
            "Recursive delete must appear exactly once, inside removeDisposableProfile. Found: " f"{rm_lines}"
        )
    # It lives in a SHARED helper because two callers need it with different retry policies:
    # cleanupProfile waits out Windows' asynchronous handle release, while die() fires before
    # anything is spawned and has nothing to wait for. That second caller is what first tried to add
    # its own `fs.rmSync` with its own copy of the guards, and this gate caught it. Pinning the
    # helper's existence stops the next person re-inlining either one.
    assert_contains(e2e, "function removeDisposableProfile()", E2E.name)
    assert_contains(e2e, "const DATA_DIR_IS_OURS = !process.env.CORTEX_APP_DATA_DIR;", E2E.name)
    assert_contains(e2e, "if (!DATA_DIR_IS_OURS) return;", E2E.name)
    # Must refuse anything that is not strictly BELOW the temp root (equality included, or a bare
    # tmpdir would be removable).
    assert_contains(e2e, "target === root || !target.startsWith(root + path.sep)", E2E.name)

    # Evidence rule: cleanup runs on the success path only. If it were also called from the failure
    # handler, a post-mortem would find the DB it needs already deleted.
    failure_handler = e2e.split("run().catch(")[-1]
    if "cleanupProfile()" in failure_handler:
        raise AssertionError("cleanupProfile must NOT run on the failure path — the profile is the evidence")
    assert_contains(e2e, "Profile kept for diagnosis", E2E.name)


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


def test_every_spawning_harness_is_isolated_from_the_production_library() -> None:
    """EVERY harness that launches the exe, not just e2e_real_app.cjs.

    The profile isolation, the PID-tree kill and the WebView2 folder were built for one harness and
    its siblings were left behind — `e2e_constrained_ipc`, `e2e_finetuned_ipc` and `e2e_pipeline_ipc`
    all spawned the app with a bare `{...process.env}`, so they ran against the owner's real
    %APPDATA% library and imported audio into a corpus holding human review decisions, then killed
    by IMAGE NAME, taking his own running Cortex with them. Same shape as a guard applied at one call
    site instead of the shared one, which is why this checks the whole set.

    e2e_real_app.cjs keeps its own (pinned above); the other three share e2e_profile.cjs.
    """
    for path in sorted(REPO_ROOT.glob("e2e_*.cjs")):
        src = path.read_text(encoding="utf-8")
        code = "\n".join(ln for ln in src.splitlines() if not ln.strip().startswith(("//", "*", "/*")))
        if "spawn(APP_EXE" not in code:
            # A connect-only harness launches nothing, so a disposable profile is not available to it:
            # it writes to whatever library the app it attached to has open — the REAL one. Isolation
            # cannot be the remedy, so an explicit acknowledgement is. Without this branch the check
            # would wave such a harness through as "not spawning, not our problem", which is exactly
            # how e2e_7b_connect went unnoticed.
            if "import_audio_file" in code and "CORTEX_ALLOW_LIVE_PROFILE" not in code:
                raise AssertionError(
                    f"{path.name} attaches to a running app and imports into its LIVE library without "
                    "requiring CORTEX_ALLOW_LIVE_PROFILE=1 — a casual run would add clips to the "
                    "owner's corpus."
                )
            continue
        if "/IM cortex-speech-app.exe" in code:
            raise AssertionError(
                f"{path.name} kills by IMAGE NAME — that takes down the owner's own running Cortex. "
                "Kill only the spawned PID tree."
            )
        if "CORTEX_APP_DATA_DIR" not in code and "launchEnv(" not in code:
            raise AssertionError(
                f"{path.name} spawns the app without isolating CORTEX_APP_DATA_DIR — it would import "
                "into the owner's REAL library. Use e2e_profile.cjs (or e2e_real_app's own guard)."
            )
        if "WEBVIEW2_USER_DATA_FOLDER" not in code and "launchEnv(" not in code:
            raise AssertionError(
                f"{path.name} spawns the app without isolating the WebView2 profile — it fails "
                "whenever the owner's app is open (HRESULT 0x8007139F)."
            )


def test_the_shared_profile_guard_refuses_the_production_directory() -> None:
    """The refusal must live in the shared module, not be re-derived per harness."""
    guard = (REPO_ROOT / "e2e_profile.cjs").read_text(encoding="utf-8")
    for needle in (
        "REFUSED: CORTEX_APP_DATA_DIR points at the REAL profile",
        "taskkill /F /T /PID",
        "WEBVIEW2_USER_DATA_FOLDER",
        "target === root || !target.startsWith(root + path.sep)",
    ):
        assert_contains(guard, needle, "e2e_profile.cjs")
    if "/IM cortex-speech-app.exe" in guard:
        raise AssertionError("e2e_profile.cjs must never kill by image name")


def main() -> None:
    test_mp4_temp_cleanup_is_constrained_to_temp()
    test_recursive_remove_only_exists_inside_safety_helper()
    test_package_real_audio_scripts_use_the_checked_runner()
    test_e2e_never_clears_the_real_db_by_default()
    test_e2e_is_isolated_from_the_production_profile()
    test_e2e_profile_cleanup_is_guarded_and_keeps_evidence_on_failure()
    test_clear_db_snapshots_before_deleting_and_requires_confirmation()
    test_every_spawning_harness_is_isolated_from_the_production_library()
    test_the_shared_profile_guard_refuses_the_production_directory()
    print("real-data runner policy regression passed")


if __name__ == "__main__":
    main()
