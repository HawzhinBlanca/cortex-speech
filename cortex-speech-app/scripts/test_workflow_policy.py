from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
# GitHub Actions runs the workflows at the GIT ROOT (one level above cortex-speech-app/), not the
# app subdir. The cortex-speech-app/.github duplicate was removed as dead config, so this policy
# validates the live root workflows that CI actually executes.
WORKFLOWS_DIR = REPO_ROOT.parent / ".github" / "workflows"
CARGO_DENY_VERSION = "0.19.8"
RUST_COVERAGE_TOOL_VERSION = "0.8.7"
RUST_COVERAGE_CONTRACT_PATH = REPO_ROOT / "scripts" / "rust_coverage_toolchain.json"
RUST_COVERAGE_CONTRACT = json.loads(RUST_COVERAGE_CONTRACT_PATH.read_text(encoding="utf-8"))
RUST_COVERAGE_TOOLCHAIN = str(RUST_COVERAGE_CONTRACT["toolchain"])
CLEAN_RELEASE_GATE_COMMANDS = [
    "npm ci",
    "npx playwright install chromium",
    "npm run typecheck",
    "npm test",
    "npm run setup:python-policies",
    "npm run test:python-policies",
    "npm run lint",
    "cargo fmt --manifest-path src-tauri/Cargo.toml --all --check",
    "cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings",
    "cargo test --manifest-path src-tauri/Cargo.toml --all-targets --all-features",
    "npm run test:e2e",
    "npm audit --omit=dev",
    "cargo deny --manifest-path src-tauri/Cargo.toml check",
]


def workflow(name: str) -> str:
    return (WORKFLOWS_DIR / name).read_text(encoding="utf-8")


def workflow_steps_text(name: str) -> str:
    """Workflow text with YAML full-line comments removed, so ``.find()`` matches REAL step
    commands rather than commands merely mentioned in an explanatory comment — e.g. ci.yml's
    ``# tauri::generate_context! (compiled by clippy / cargo test below) ...`` note would otherwise
    register as an earlier ``cargo test`` step and make the ordering check false-fire."""
    return "\n".join(line for line in workflow(name).splitlines() if not line.lstrip().startswith("#"))


def release_docs() -> str:
    return (REPO_ROOT / "docs" / "RELEASE.md").read_text(encoding="utf-8")


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def test_workflow_yaml_is_ascii() -> None:
    offenders: list[str] = []
    for path in sorted(WORKFLOWS_DIR.glob("*.yml")):
        for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if any(ord(char) > 127 for char in line):
                offenders.append(f"{path.name}:{line_no}:{ascii(line)}")
    if offenders:
        raise AssertionError("Workflow YAML must stay ASCII-clean:\n" + "\n".join(offenders))


def test_workflow_permissions_are_explicit() -> None:
    expectations = {
        "ci.yml": "contents: read",
        "nightly-real-audio.yml": "contents: read",
        "release.yml": "contents: write",
    }
    for name, expected in expectations.items():
        text = workflow(name)
        assert_contains(text, "permissions:", name)
        assert_contains(text, expected, name)


MAX_TIMEOUT_MINUTES = 180


def _job_timeouts(text: str, name: str) -> dict[str, int | None]:
    """Job id -> its timeout-minutes (None when it has none).

    Text-based on purpose. This suite runs on the Linux and macOS CI runners too, and pulling in
    PyYAML to read three files would trade a real portability risk for a small convenience -- on
    2026-08-08 two Windows-only policies were exactly what kept both Build Smoke gates red for over
    a week. Job ids are the keys at two-space indent inside the top-level `jobs:` block, their
    properties sit at four, and the `on:`/`permissions:`/`env:` blocks are excluded by starting the
    scan at `jobs:` and stopping at the next column-0 key.
    """
    lines = text.splitlines()
    start = next((i for i, ln in enumerate(lines) if ln.rstrip() == "jobs:"), None)
    if start is None:
        raise AssertionError(f"{name}: no top-level `jobs:` block")

    jobs: dict[str, int | None] = {}
    current: str | None = None
    for line in lines[start + 1 :]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            break
        if indent == 2 and line.rstrip().endswith(":"):
            current = line.strip().rstrip(":")
            jobs.setdefault(current, None)
        elif current and indent == 4 and line.strip().startswith("timeout-minutes:"):
            jobs[current] = int(line.split(":", 1)[1].strip())
    return jobs


def test_workflow_jobs_have_timeouts() -> None:
    """EVERY job carries a timeout, and none is absurdly long.

    This used to pin exact values ("timeout-minutes: 75"), which made it a change-detector rather
    than the policy its name claims. Two problems with that. It broke whenever a timeout moved for a
    legitimate reason -- raising the nightly's 75, which had been killing the soak mid-run every
    night, tripped it. And it was far too weak in the direction that matters: it only asked whether
    those strings appeared ANYWHERE in the file, so a NEWLY ADDED job with no timeout at all sailed
    through. That is the real hazard - an untimed job hangs until GitHub's six-hour ceiling.

    So: assert the invariant per job, and cap it, which keeps "just raise the timeout" from being a
    silent way to hide something that hangs.
    """
    for path in sorted(WORKFLOWS_DIR.glob("*.yml")):
        jobs = _job_timeouts(path.read_text(encoding="utf-8"), path.name)
        if not jobs:
            raise AssertionError(f"{path.name}: parsed no jobs at all - the scan is broken")
        for job, minutes in sorted(jobs.items()):
            if minutes is None:
                raise AssertionError(
                    f"{path.name}: job `{job}` has no timeout-minutes. Without one a wedged step "
                    f"runs until GitHub's 6-hour ceiling and burns a runner for nothing."
                )
            if minutes > MAX_TIMEOUT_MINUTES:
                raise AssertionError(
                    f"{path.name}: job `{job}` allows {minutes} min, over the "
                    f"{MAX_TIMEOUT_MINUTES} min cap. If a job genuinely needs longer, make the work "
                    f"smaller or split the job - do not raise the ceiling to hide a hang."
                )


def test_cargo_deny_install_is_pinned() -> None:
    expected = f"cargo install cargo-deny --version {CARGO_DENY_VERSION} --locked"
    for name in ["ci.yml", "release.yml"]:
        assert_contains(workflow(name), expected, name)
    assert_contains(release_docs(), expected, "docs/RELEASE.md")


def test_dependency_advisories_cannot_be_suppressed() -> None:
    """A documented ignore is still a green result with a known advisory hidden behind it.

    Owner-workstation certification requires the dependency graph to move to a patched release or
    stay red.  Keeping this as an exact empty list also prevents cargo-deny defaults from changing
    the meaning of an omitted field under a later tool version.
    """

    policy = tomllib.loads((REPO_ROOT / "deny.toml").read_text(encoding="utf-8"))
    advisories = policy.get("advisories")
    if not isinstance(advisories, dict) or advisories.get("ignore") != []:
        raise AssertionError("deny.toml must carry an explicit empty advisory ignore list")


def test_champion_launcher_escapes_windows_path_separator_regex() -> None:
    """PowerShell ``-replace`` treats its first operand as a regular expression.

    A single backslash is a syntactically incomplete regex and makes the supported owner launcher
    abort before it can even read the champion pointer.  Both Windows paths converted for WSL must
    therefore use the literal-backslash regex ``\\``.  This is deliberately checked as a source
    invariant because executing the launcher in policy tests would load the 7B model.
    """

    launcher = (REPO_ROOT / "scripts" / "start_7b_server.ps1").read_text(encoding="utf-8")
    invalid = r"-replace '\', '/'"
    valid = r"-replace '\\', '/'"
    if invalid in launcher:
        raise AssertionError("start_7b_server.ps1 contains PowerShell's invalid single-backslash regex")
    if launcher.count(valid) != 1:
        raise AssertionError("champion launcher must centralize the valid backslash regex in one converter")
    for conversion in (
        "ConvertTo-WslPath $winPointer",
        "ConvertTo-WslPath $serverWin",
        "ConvertTo-WslPath $clientWin",
        "ConvertTo-WslPath $guardWin",
    ):
        if conversion not in launcher:
            raise AssertionError(f"champion launcher bypasses its checked path converter: {conversion}")

    for durability_guard in (
        "Start-Process -FilePath \"wsl.exe\" -WindowStyle Hidden -PassThru",
        "Invoke-WslBashProgram",
        "-RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath",
        "$null = $process.Handle",
        "$process.WaitForExit([int]($TimeoutSeconds * 1000))",
        'Stop-Process -InputObject $process -Force',
        'ConvertTo-BashLiteral "CORTEX_7B_HEALTH_TIMEOUT_SECONDS=5"',
        '"/usr/bin/env", "-u", "BASH_ENV", "-u", "ENV"',
        '"/bin/bash", "--noprofile", "--norc", "-c"',
        "/usr/bin/base64 -d | /bin/bash --noprofile --norc",
        "[Diagnostics.Stopwatch]::StartNew()",
        "$serverProcess.HasExited",
        "$null = $serverProcess.Handle",
        "cortex_7b_client.py",
        "--health",
        "--expected-pointer",
        "cortex_7b_launch_guard.py",
        'Set-LaunchState "heartbeat"',
        'Set-LaunchState "ready"',
        'Set-LaunchState "stop"',
        "--heartbeat-timeout 45",
        'New-Object System.Threading.Mutex($false, "Local\\CortexSpeechChampionLaunch-$port")',
        "[System.Threading.AbandonedMutexException]",
        "$launchMutex.ReleaseMutex()",
        '$launchToken = [Guid]::NewGuid().ToString("N")',
        "$retainServer = $false",
        "$retainServer = $true",
        "finally {",
        "Stop-Process -InputObject $serverProcess -Force",
    ):
        if durability_guard not in launcher:
            raise AssertionError(f"champion launcher is missing durable pointer-bound startup: {durability_guard}")
    launcher_code = "\n".join(
        line for line in launcher.splitlines() if not line.lstrip().startswith("#")
    )
    health_invocations = [line for line in launcher_code.splitlines() if "$healthProgram =" in line]
    if len(health_invocations) != 1 or not all(
        marker in health_invocations[0]
        for marker in ("$clientWsl)", "--health", "--expected-pointer", "$pointer)")
    ):
        raise AssertionError("launcher must have one health path and bind it to the current pointer")
    if launcher_code.count("Invoke-WslBashProgram -Program $healthProgram -TimeoutSeconds 15") != 1:
        raise AssertionError("launcher health must cross WSL through one explicitly bounded invocation")
    if launcher_code.count('Start-Process -FilePath "wsl.exe"') != 2:
        raise AssertionError("launcher must have only the bounded WSL helper and exact guarded server process")
    if re.search(r"(?m)^\s*(?:&\s+)?wsl\.exe\b", launcher_code):
        raise AssertionError("launcher contains an unbounded direct wsl.exe invocation")
    if "Get-Date" in launcher_code:
        raise AssertionError("launcher deadlines must use a monotonic Stopwatch, not adjustable wall time")
    for private_output in (
        'Write-Host "  script:    $serverWsl"',
        'Write-Host "  pointer:   $pointer',
        'WriteLine("no champion pointer at $winPointer',
        'WriteLine("server script not found: $serverWin',
        'WriteLine("client script not found: $clientWin',
        'WriteLine("launch guard not found: $guardWin',
        'throw "could not convert Windows path for WSL: $WindowsPath"',
    ):
        if private_output in launcher_code:
            raise AssertionError("launcher status/error output must not retain absolute private paths")
    if launcher_code.count("$retainServer = $true") != 1:
        raise AssertionError("launcher must have exactly one ownership-transfer point after readiness")
    poll_loop = launcher_code.find("$startupTimer = [Diagnostics.Stopwatch]::StartNew()")
    heartbeat = launcher_code.find('Set-LaunchState "heartbeat"', poll_loop)
    sleep = launcher_code.find("Start-Sleep -Seconds 10", heartbeat)
    bound_health = launcher_code.find("if (Test-ServerReady)", sleep)
    ready_signal = launcher_code.find('Set-LaunchState "ready"', bound_health)
    ownership_transfer = launcher_code.find("$retainServer = $true", ready_signal)
    stop_signal = launcher_code.find('Set-LaunchState "stop"', ownership_transfer)
    forced_stop = launcher_code.find("Stop-Process -InputObject $serverProcess -Force", stop_signal)
    if min(
        poll_loop,
        heartbeat,
        sleep,
        bound_health,
        ready_signal,
        ownership_transfer,
        stop_signal,
        forced_stop,
    ) < 0 or not (
        poll_loop
        < heartbeat
        < sleep
        < bound_health
        < ready_signal
        < ownership_transfer
        < stop_signal
        < forced_stop
    ):
        raise AssertionError(
            "launcher ownership must be heartbeat -> pointer-bound health -> READY transfer, "
            "with exact guard stop before forced WSL cleanup"
        )
    mutex = launcher_code.find("Local\\CortexSpeechChampionLaunch-$port")
    mutex_wait = launcher_code.find("$launchMutex.WaitOne(0)", mutex)
    mutex_recheck = launcher_code.find("if (Test-ServerReady)", mutex_wait)
    child_start = launcher_code.find('Start-Process -FilePath "wsl.exe"', mutex_recheck)
    if min(mutex, mutex_wait, mutex_recheck, child_start) < 0 or not (
        mutex < mutex_wait < mutex_recheck < child_start
    ):
        raise AssertionError("concurrent launch mutex must recheck exact readiness before child start")
    if "> ~/cortex_7b_server.log" in launcher_code:
        raise AssertionError("launcher must not truncate a shared log during concurrent starts")
    if '"-lc"' in launcher_code:
        raise AssertionError("launcher must not source an interactive/login Bash profile before startup")
    if "nohup" in launcher_code:
        raise AssertionError("champion launcher must not orphan the model behind a short-lived WSL process")


def test_champion_launcher_parses_in_an_available_powershell() -> None:
    """Use the real parser on Windows/release hosts; source checks cannot prove brace correctness."""
    executable = shutil.which("powershell.exe") or shutil.which("pwsh")
    if executable is None:
        return
    environment = os.environ.copy()
    environment["CORTEX_LAUNCHER_POLICY_PATH"] = str(REPO_ROOT / "scripts" / "start_7b_server.ps1")
    parser = (
        "$tokens=$null;$errors=$null;"
        "[void][System.Management.Automation.Language.Parser]::ParseFile("
        "$env:CORTEX_LAUNCHER_POLICY_PATH,[ref]$tokens,[ref]$errors);"
        "if($errors.Count){$errors|ForEach-Object{[Console]::Error.WriteLine($_.Message)};exit 1}"
    )
    result = subprocess.run(
        [executable, "-NoProfile", "-Command", parser],
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(f"champion launcher PowerShell parse failed: {result.stderr or result.stdout}")
    invalid_environment = environment.copy()
    invalid_environment["CORTEX_7B_PORT"] = "not-a-port"
    invalid = subprocess.run(
        [
            executable,
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            environment["CORTEX_LAUNCHER_POLICY_PATH"],
        ],
        env=invalid_environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if invalid.returncode != 2 or "must be an integer from 1 through 65535" not in invalid.stderr:
        raise AssertionError("champion launcher did not fail closed before WSL on an invalid port")


def test_rust_quality_authorities_are_split_mandatory_and_fail_closed() -> None:
    """Coverage is a separately supervised prerequisite, not an overlong in-process gate.

    The consumer gate must explicitly fail when the prerequisite is skipped or fails; relying on
    GitHub's default `needs` skip semantics would let the required Windows status appear without an
    actual coverage verdict. The diagnostic coverage-json mode is never certifying.
    """

    install = f"cargo install cargo-llvm-cov --version {RUST_COVERAGE_TOOL_VERSION} --locked"
    install_toolchain = (
        f"rustup toolchain install {RUST_COVERAGE_TOOLCHAIN} "
        "--profile minimal --component llvm-tools-preview"
    )
    prerequisite = 'python "${{ github.workspace }}/scripts/verify_10.py" --rust-coverage-prerequisite'
    architecture = "python scripts/rust_quality_gate.py architecture"
    for name in ["ci.yml", "release.yml"]:
        text = workflow_steps_text(name)
        if text.count(install) != 1:
            raise AssertionError(f"{name} must install exactly one pinned cargo-llvm-cov authority")
        if text.count(install_toolchain) != 1:
            raise AssertionError(f"{name} must install exactly one date-pinned coverage nightly")
        if "rustup toolchain install nightly " in text or "toolchain: nightly" in text:
            raise AssertionError(f"{name} must never resolve coverage through rolling nightly")
        assert_contains(
            text,
            "cortex-speech-app/scripts/rust_coverage_toolchain.json",
            f"{name} coverage cache authority",
        )
        if text.count(prerequisite) != 1:
            raise AssertionError(f"{name} must run exactly one supervised Rust coverage prerequisite")
        if text.count(architecture) != 1:
            raise AssertionError(f"{name} must run exactly one independent Rust architecture gate")
        for required in (
            "cargo fetch --locked --manifest-path src-tauri/Cargo.toml",
            "npm run fetch-models",
            "npm run build",
        ):
            assert_contains(text, required, f"{name} coverage provisioning")
        if "coverage-json" in text or "continue-on-error" in text:
            raise AssertionError(f"{name} contains a non-certifying or non-blocking Rust truth path")
    assert_contains(release_docs(), install_toolchain, "docs/RELEASE.md coverage nightly")

    ci = workflow_steps_text("ci.yml")
    for guard in (
        "needs: rust-coverage-prerequisite",
        "if: ${{ always() }}",
        "RUST_COVERAGE_RESULT: ${{ needs.rust-coverage-prerequisite.result }}",
        "Windows Release Gate refuses a missing, skipped, cancelled, or failed Rust coverage prerequisite.",
    ):
        assert_contains(ci, guard, "ci.yml coverage prerequisite consumer")
    prerequisite_job = ci.find("  rust-coverage-prerequisite:")
    windows_job = ci.find("  windows-release-gate:")
    prerequisite_run = ci.find("--rust-coverage-prerequisite", prerequisite_job)
    refusal = ci.find("Windows Release Gate refuses", windows_job)
    architecture_run = ci.find(architecture, windows_job)
    if min(prerequisite_job, windows_job, prerequisite_run, refusal, architecture_run) < 0 or not (
        prerequisite_job < prerequisite_run < windows_job < refusal < architecture_run
    ):
        raise AssertionError("CI must complete coverage before the independently blocking architecture/source gate")

    release = workflow_steps_text("release.yml")
    for guard in (
        "needs: rust-coverage-prerequisite",
        "needs.rust-coverage-prerequisite.result == 'success'",
    ):
        assert_contains(release, guard, "release.yml coverage prerequisite consumer")
    prerequisite_job = release.find("  rust-coverage-prerequisite:")
    build_job = release.find("  build:")
    prerequisite_run = release.find("--rust-coverage-prerequisite", prerequisite_job)
    architecture_run = release.find(architecture, build_job)
    if min(prerequisite_job, build_job, prerequisite_run, architecture_run) < 0 or not (
        prerequisite_job < prerequisite_run < build_job < architecture_run
    ):
        raise AssertionError("release build must be unreachable until coverage passes, then run architecture independently")

    production_toolchain = (REPO_ROOT.parent / "rust-toolchain.toml").read_text(encoding="utf-8")
    if 'channel = "1.95.0"' not in production_toolchain or "nightly" in production_toolchain:
        raise AssertionError("the production Rust toolchain must remain exact stable 1.95.0")


def test_release_docs_and_tag_workflow_share_clean_gate() -> None:
    docs = release_docs()
    release = workflow("release.yml")
    for command in CLEAN_RELEASE_GATE_COMMANDS:
        assert_contains(docs, f"`{command}`", "docs/RELEASE.md")
        if command == "npx playwright install chromium":
            assert_contains(release, "npx playwright install", "release.yml")
            assert_contains(release, "chromium", "release.yml")
        else:
            assert_contains(release, command, "release.yml")
    assert_contains(release, "npm run tauri build", "release.yml")
    for contract_text in (
        "Add five repository secrets",
        "RULESET_AUDIT_TOKEN",
        "WINDOWS_CERT_BASE64",
        "WINDOWS_CERT_PASSWORD",
        "WINDOWS_CERT_THUMBPRINT",
        "WINDOWS_CERT_SHA256",
        "SHA256SUMS-windows-11-x64",
        "refs/tags/v*",
        "signtool verify /pa /all /v /tw",
    ):
        assert_contains(docs, contract_text, "docs/RELEASE.md release identity contract")
    if "either signing secret" in docs:
        raise AssertionError("docs/RELEASE.md still describes the retired two-secret signing contract")


def test_playwright_browser_install_precedes_e2e() -> None:
    ci = workflow("ci.yml")
    ci_install = ci.find("npx playwright install chromium")
    ci_e2e = ci.find("npm run test:e2e")
    if ci_install < 0 or ci_e2e < 0 or ci_install > ci_e2e:
        raise AssertionError("ci.yml must install Playwright Chromium before npm run test:e2e")

    release = workflow("release.yml")
    release_install = release.find("npx playwright install")
    release_e2e = release.find("npm run test:e2e")
    if release_install < 0 or release_e2e < 0 or release_install > release_e2e:
        raise AssertionError("release.yml must install Playwright Chromium before npm run test:e2e")


def test_locked_python_setup_precedes_every_policy_run() -> None:
    setup = "npm run setup:python-policies"
    run = "npm run test:python-policies"
    for name in ["ci.yml", "release.yml"]:
        text = workflow_steps_text(name)
        cursor = 0
        runs = 0
        while True:
            run_index = text.find(run, cursor)
            if run_index < 0:
                break
            setup_index = text.rfind(setup, cursor, run_index)
            if setup_index < 0:
                raise AssertionError(
                    f"{name}: every Python policy run needs a preceding locked-environment setup"
                )
            runs += 1
            cursor = run_index + len(run)
        if runs == 0:
            raise AssertionError(f"{name}: no Python policy run found")


def test_provisioning_precedes_every_compiling_cargo_step() -> None:
    """The failure class that broke main twice in two days (2026-07-08 Release Gate, 2026-07-09
    Nightly): any workflow step that COMPILES the crate runs build.rs -> tauri-build, which
    validates bundle.resources (needs `npm run fetch-models`) and compiles
    tauri::generate_context!, which needs a built ../dist (`npm run build`). Presence is not
    enough - ORDER is the contract. This gate-on-the-gate previously asserted presence only, so a
    workflow that could not run was still policy-green (true-10 audit 2026-07-09)."""
    compiling = ["cargo fmt", "cargo clippy", "cargo test", "cargo build"]
    for name in ["ci.yml", "release.yml", "nightly-real-audio.yml"]:
        text = workflow_steps_text(name)  # comment lines stripped so a comment can't pose as a step
        first_cargo = min((idx for idx in (text.find(cmd) for cmd in compiling) if idx >= 0), default=-1)
        if first_cargo < 0:
            continue
        fetch = text.find("npm run fetch-models")
        if fetch < 0:
            raise AssertionError(f"{name} compiles the crate but never runs `npm run fetch-models`")
        if fetch > first_cargo:
            raise AssertionError(
                f"{name}: `npm run fetch-models` must come BEFORE the first compiling cargo step "
                f"(build.rs validates bundle.resources)"
            )
        # Plain substring is unambiguous: "npm run tauri build" does not contain "npm run build",
        # and a newline-suffixed pattern would be CRLF-fragile.
        build = text.find("npm run build")
        if build < 0:
            raise AssertionError(f"{name} compiles the crate but never builds the frontend (`npm run build`)")
        if build > first_cargo:
            raise AssertionError(
                f"{name}: `npm run build` must come BEFORE the first compiling cargo step "
                f"(tauri::generate_context! needs ../dist)"
            )


def test_release_runs_the_governance_gate() -> None:
    """Tags can be cut from ANY commit, so release.yml must run verify_10.py itself (manifest/
    version alignment, ledger schema, license compatibility) rather than assume ci.yml did
    (true-10 audit 2026-07-09)."""
    assert_contains(workflow("release.yml"), "verify_10.py", "release.yml")


def test_windows_ci_covers_every_rust_target_and_feature() -> None:
    ci = workflow_steps_text("ci.yml")
    assert_contains(
        ci,
        "cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings",
        "ci.yml Windows clippy gate",
    )
    assert_contains(
        ci,
        "cargo test --manifest-path src-tauri/Cargo.toml --all-targets --all-features",
        "ci.yml Windows Rust test gate",
    )


def test_bundle_contract_is_explicitly_windows_only_and_has_no_unsigned_updater() -> None:
    config = json.loads((REPO_ROOT / "src-tauri" / "tauri.conf.json").read_text(encoding="utf-8"))
    bundle = config.get("bundle", {})
    if bundle.get("targets") != ["msi", "nsis"]:
        raise AssertionError("the public bundle contract must contain exactly the Windows MSI and NSIS targets")
    if bundle.get("createUpdaterArtifacts") is not False:
        raise AssertionError(
            "updater artifacts must remain explicitly disabled until a pinned public key, endpoint, "
            "private signing-key workflow, and update/rollback proof are implemented"
        )


def test_release_fails_closed_on_signing_and_attests_artifacts() -> None:
    """A public tag must never degrade to an unsigned Windows download, and every published
    artifact must carry both an offline-verifiable digest and GitHub build provenance."""
    release = workflow_steps_text("release.yml")
    assert_contains(release, "id-token: write", "release.yml")
    assert_contains(release, "attestations: write", "release.yml")
    assert_contains(release, "artifact-metadata: write", "release.yml")
    assert_contains(release, "refusing to publish unsigned or wrong-publisher Windows installers", "release.yml")
    if "Installers UNSIGNED" in release or "exit 0" in release:
        raise AssertionError("release.yml must fail closed when Windows signing credentials are missing")
    for signed_tag_guard in (
        "fetch-depth: 0",
        "GitHub did not cryptographically verify the annotated release tag signature",
        "$tagObject.verification.verified",
        "rulesets?includes_parents=true&targets=tag",
        "RULESET_AUDIT_TOKEN: ${{ secrets.RULESET_AUDIT_TOKEN }}",
        "$rulesetHeaders.Authorization = \"Bearer $env:RULESET_AUDIT_TOKEN\"",
        "$null -eq $bypassProperty",
        "refusing to treat hidden authority as an empty bypass list",
        "$includes -contains 'refs/tags/v*'",
        "$bypassActors.Count -eq 0",
        "$ruleTypes -contains 'deletion'",
        "$ruleTypes -contains 'non_fast_forward'",
        "rulesets?includes_parents=true&targets=branch",
        "$repository.default_branch -ne 'main'",
        "$includes -contains 'refs/heads/main'",
        "$includes -contains '~DEFAULT_BRANCH'",
        "$ruleTypes -contains 'required_signatures'",
        "$pullRequestRule.parameters.required_approving_review_count -ge 1",
        "$pullRequestRule.parameters.dismiss_stale_reviews_on_push -eq $true",
        "$pullRequestRule.parameters.require_last_push_approval -eq $true",
        "$pullRequestRule.parameters.required_review_thread_resolution -eq $true",
        "$statusChecksRule.parameters.strict_required_status_checks_policy -eq $true",
        "$requiredStatusContexts -contains 'Provenance & License Gate'",
        "$requiredStatusContexts -contains 'Windows Release Gate'",
        "protected-main authority is unprovable",
        "$tagObject.object.sha.ToLowerInvariant() -ne $head",
        "git merge-base --is-ancestor $head origin/main",
        'if ($tag -ne "v$packageVersion")',
    ):
        assert_contains(release, signed_tag_guard, "release.yml signed-tag gate")
    if 'Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$env:GITHUB_REPOSITORY/rulesets' in release:
        raise AssertionError(
            "release.yml must use the privileged, repository-scoped audit token for every ruleset "
            "request; the normal workflow token can receive a response with bypass_actors omitted"
        )
    assert_contains(release, "scripts/generate_release_checksums.py", "release.yml")
    assert_contains(release, "actions/attest@59d89421af93a897026c735860bf21b6eb4f7b26", "release.yml")
    assert_contains(release, "needs: build", "release.yml")
    assert_contains(release, "actions/download-artifact@018cc2cf5baa6db3ef3c5f8a56943fffe632ef53", "release.yml")
    assert_contains(release, "name: cortex-speech-windows-11-x64", "release.yml")
    assert_contains(release, "runs-on: windows-latest", "release.yml")
    if "matrix.os" in release or "macos-latest" in release or "runs-on: ubuntu-latest" in release[: release.find("  publish:")]:
        raise AssertionError("the public build job must produce only the supported Windows 11 x64 bundle")
    assert_contains(release, "signtool", "release.yml")
    assert_contains(release, "verify /pa /all /v /tw", "release.yml")
    assert_contains(release, "WINDOWS_CERT_THUMBPRINT", "release.yml")
    assert_contains(release, "WINDOWS_CERT_SHA256", "release.yml")
    assert_contains(release, "$msi.Count -ne 1 -or $nsis.Count -ne 1", "release.yml")
    assert_contains(release, "$actualThumbprint -ne $expectedThumbprint", "release.yml")
    assert_contains(release, "$actualCertificateSha256 -ne $expectedCertificateSha256", "release.yml")
    assert_contains(release, "if-no-files-found: error", "release.yml")
    assert_contains(release, "scripts/windows_release_bundle.py", "release.yml")
    assert_contains(release, "--verify-authenticode", "release.yml")
    assert_contains(release, "--verify-provenance", "release.yml")
    assert_contains(release, "release package inventory is not exact", "release.yml")
    assert_contains(release, "release-environment.json", "release.yml")
    assert_contains(release, 'release environment source identity disagrees with the workflow SHA', "release.yml")
    assert_contains(release, '--expected-sha "$env:GITHUB_SHA"', "release.yml")
    assert_contains(release, "gh attestation verify", "release.yml")
    assert_contains(release, "--signer-workflow", "release.yml")
    assert_contains(release, '--source-digest "$env:GITHUB_SHA"', "release.yml")
    assert_contains(release, '--source-ref "$env:GITHUB_REF"', "release.yml")
    assert_contains(release, "fail_on_unmatched_files: true", "release.yml")
    assert_contains(release, "draft: true", "release.yml uncertified-publication fence")
    if "draft: false" in release:
        raise AssertionError("release.yml must not publish a stable release before windows-product proof is consumed")
    for proof_guard in (
        "workflow_dispatch:",
        "proof_run_id:",
        "--require-certifying-proof",
        "--profile windows-product",
        "--proof-manifest",
        "--expected-sha",
        "--windows-release-bundle",
        "certifying proof or exact artifact binding was rejected",
        "draft asset identity changed after proof consumption",
        "github.ref == 'refs/heads/main'",
        "PROMOTION_WORKFLOW_REF: ${{ github.workflow_ref }}",
        "$env:GITHUB_REPOSITORY/.github/workflows/release.yml@refs/heads/main",
        "Stable promotion must execute the exact release workflow from protected main.",
        "Immutable no-bypass tag authority changed after proof consumption.",
        "Signed release tag changed after proof consumption.",
        "gh release edit $env:RELEASE_TAG --draft=false --latest",
    ):
        assert_contains(release, proof_guard, "release.yml certifying-proof consumer")
    proof_consumer = release.find("--require-certifying-proof")
    final_tag_recheck = release.find("Signed release tag changed after proof consumption.")
    stable_publication = release.find("--draft=false")
    if (
        proof_consumer < 0
        or final_tag_recheck < 0
        or stable_publication < 0
        or not proof_consumer < final_tag_recheck < stable_publication
        or release.count("--draft=false") != 1
    ):
        raise AssertionError(
            "stable publication must occur only after exact proof consumption and a final signed-tag recheck"
        )
    for path in sorted(WORKFLOWS_DIR.glob("*.yml")):
        if path.name == "release.yml":
            continue
        other = path.read_text(encoding="utf-8")
        if "action-gh-release" in other or "gh release create" in other or "--draft=false" in other:
            raise AssertionError(f"{path.name} contains an alternate release-publication path")

    no_bundle = release.find("npm run tauri build -- --no-bundle")
    app_sign = release.find("$st sign /f $pfx", no_bundle)
    bundle = release.find("npm run tauri bundle -- --bundles msi,nsis")
    installer_sign = release.find("$st sign /f $pfx", app_sign + 1)
    if min(no_bundle, app_sign, bundle, installer_sign) < 0 or not (
        no_bundle < app_sign < bundle < installer_sign
    ):
        raise AssertionError(
            "the app executable must be signed before bundling and installers signed afterward"
        )

    assert_contains(release, "scripts/generate_sbom.py", "release.yml")
    sbom = release.find("scripts/generate_sbom.py")
    checksums = release.find("scripts/generate_release_checksums.py")
    attestation = release.find("actions/attest@")
    upload = release.find("actions/upload-artifact@")
    publish = release.find("softprops/action-gh-release@")
    # The SBOM is written INTO the bundle, so it must precede the checksum manifest and the
    # attestation or it ships uncovered by either — an SBOM nobody can verify came from this build.
    if min(sbom, checksums, attestation, upload, publish) < 0 or not (
        sbom < checksums < attestation < upload < publish
    ):
        raise AssertionError("SBOM, checksums and provenance must be generated before upload and publication")

    build_job = release.find("  build:")
    publish_job = release.find("  publish:")
    write_permission = release.find("      contents: write")
    if min(build_job, publish_job, write_permission) < 0 or not (build_job < publish_job < write_permission):
        raise AssertionError("repository write permission must be scoped to the serialized publish job")


def test_nightly_real_audio_fails_on_real_regressions_but_skips_missing_fixtures() -> None:
    nightly = workflow("nightly-real-audio.yml")
    if "continue-on-error" in nightly:
        raise AssertionError("nightly real-audio must fail when configured tests fail")
    # Missing-fixture branch must SKIP (exit 0) but make the skip VISIBLE via a warning annotation,
    # so a green nightly is never mistaken for "real audio passed" (a skip is not a pass).
    assert_contains(nightly, "::warning title=Real-audio suite skipped", "nightly-real-audio.yml")
    assert_contains(nightly, "exit 0", "nightly-real-audio.yml")
    assert_contains(nightly, "cargo test --test real_audio -- --ignored --nocapture", "nightly-real-audio.yml")
    assert_contains(nightly, "cargo test --test soak -- --nocapture", "nightly-real-audio.yml")


def main() -> None:
    test_workflow_yaml_is_ascii()
    test_workflow_permissions_are_explicit()
    test_workflow_jobs_have_timeouts()
    test_cargo_deny_install_is_pinned()
    test_dependency_advisories_cannot_be_suppressed()
    test_champion_launcher_escapes_windows_path_separator_regex()
    test_champion_launcher_parses_in_an_available_powershell()
    test_rust_quality_authorities_are_split_mandatory_and_fail_closed()
    test_release_docs_and_tag_workflow_share_clean_gate()
    test_playwright_browser_install_precedes_e2e()
    test_locked_python_setup_precedes_every_policy_run()
    test_provisioning_precedes_every_compiling_cargo_step()
    test_release_runs_the_governance_gate()
    test_windows_ci_covers_every_rust_target_and_feature()
    test_bundle_contract_is_explicitly_windows_only_and_has_no_unsigned_updater()
    test_release_fails_closed_on_signing_and_attests_artifacts()
    test_nightly_real_audio_fails_on_real_regressions_but_skips_missing_fixtures()
    print("workflow policy regression passed")


if __name__ == "__main__":
    main()
