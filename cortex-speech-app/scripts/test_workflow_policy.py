from __future__ import annotations

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


def test_release_fails_closed_on_signing_and_attests_artifacts() -> None:
    """A public tag must never degrade to an unsigned Windows download, and every published
    artifact must carry both an offline-verifiable digest and GitHub build provenance."""
    release = workflow_steps_text("release.yml")
    assert_contains(release, "id-token: write", "release.yml")
    assert_contains(release, "attestations: write", "release.yml")
    assert_contains(release, "refusing to publish unsigned Windows installers", "release.yml")
    if "Installers UNSIGNED" in release or "exit 0" in release:
        raise AssertionError("release.yml must fail closed when Windows signing credentials are missing")
    assert_contains(release, "scripts/generate_release_checksums.py", "release.yml")
    assert_contains(release, "actions/attest@59d89421af93a897026c735860bf21b6eb4f7b26", "release.yml")
    assert_contains(release, "needs: build", "release.yml")
    assert_contains(release, "actions/download-artifact@018cc2cf5baa6db3ef3c5f8a56943fffe632ef53", "release.yml")
    assert_contains(release, "pattern: cortex-speech-windows-latest", "release.yml")
    if release.count("if: runner.os == 'Windows'") < 4:
        raise AssertionError("only the supported signed Windows bundle may enter the public release path")

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
    test_release_docs_and_tag_workflow_share_clean_gate()
    test_playwright_browser_install_precedes_e2e()
    test_provisioning_precedes_every_compiling_cargo_step()
    test_release_runs_the_governance_gate()
    test_release_fails_closed_on_signing_and_attests_artifacts()
    test_nightly_real_audio_fails_on_real_regressions_but_skips_missing_fixtures()
    print("workflow policy regression passed")


if __name__ == "__main__":
    main()
