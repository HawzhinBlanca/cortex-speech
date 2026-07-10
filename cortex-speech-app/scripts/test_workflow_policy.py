from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
# GitHub Actions runs the workflows at the GIT ROOT (one level above cortex-speech-app/), not the
# app subdir. The cortex-speech-app/.github duplicate was removed as dead config, so this policy
# validates the live root workflows that CI actually executes.
WORKFLOWS_DIR = REPO_ROOT.parent / ".github" / "workflows"
CARGO_DENY_VERSION = "0.19.8"
CLEAN_RELEASE_GATE_COMMANDS = [
    "npm ci",
    "npx playwright install chromium",
    "npm run typecheck",
    "npm test",
    "npm run test:python-policies",
    "npm run lint",
    "cargo fmt --manifest-path src-tauri/Cargo.toml --all --check",
    "cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings",
    "cargo test --manifest-path src-tauri/Cargo.toml",
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


def test_workflow_jobs_have_timeouts() -> None:
    expectations = {
        "ci.yml": ["timeout-minutes: 90", "timeout-minutes: 45"],
        "nightly-real-audio.yml": ["timeout-minutes: 75"],
        "release.yml": ["timeout-minutes: 120"],
    }
    for name, expected_values in expectations.items():
        text = workflow(name)
        for expected in expected_values:
            assert_contains(text, expected, name)


def test_cargo_deny_install_is_pinned() -> None:
    expected = f"cargo install cargo-deny --version {CARGO_DENY_VERSION} --locked"
    for name in ["ci.yml", "release.yml"]:
        assert_contains(workflow(name), expected, name)
    assert_contains(release_docs(), expected, "docs/RELEASE.md")


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
    test_release_docs_and_tag_workflow_share_clean_gate()
    test_playwright_browser_install_precedes_e2e()
    test_provisioning_precedes_every_compiling_cargo_step()
    test_release_runs_the_governance_gate()
    test_nightly_real_audio_fails_on_real_regressions_but_skips_missing_fixtures()
    print("workflow policy regression passed")


if __name__ == "__main__":
    main()
