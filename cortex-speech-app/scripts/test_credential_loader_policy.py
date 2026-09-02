#!/usr/bin/env python3
"""Nothing may hand-parse `secrets.env`. Credentials load through `ApiKeys`, which decrypts.

MEASURED 2026-08-11. `secrets.env` values may be plaintext OR `dpapi:<base64>` — DPAPI-encrypted at
rest, which is exactly what `set_api_key` writes. `tests/openrouter_live.rs` read the file itself,
stripped `NAME=` off the line, and used whatever followed. The day the stored OpenRouter key was
encrypted (same value, verified byte-identical by hash) that test began sending the CIPHERTEXT as a
bearer token and failed with `status code 401`, reddening the `ignored-real-model` gate — while the
app kept refining perfectly, because every production path goes through `ApiKeys::load`.

That is the whole failure mode: a second, inferior credential reader that works only until the real
one improves. The bug is not "the test broke", it is "a reader existed that did not understand the
format". `ApiKeys::load` handles both encodings and is the only thing that should ever read that file.

Scope: Rust sources and tests, excluding api_keys.rs (the real loader) and dpapi.rs (the codec it
uses). A mention of the filename in a message or doc comment is fine — this looks for actual parsing.

Added 2026-09-02: the loader also overlays the PROCESS ENVIRONMENT, so a shell that exports a key
would hand it to every test binary. Isolation lives in two places -- ci.yml blanks both names at the
top level (a blank variable counts as unset in the loader: it neither overrides nor clears a stored
key), and the library test binary skips the overlay under cfg(test). It must NOT live in a cargo
config outside src-tauri: the owner-proof helper build refuses any such file. The third test pins
all three facts; `api_keys::tests` pins the blank-is-unset rule.
"""

from __future__ import annotations

import re
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src-tauri"
REPO_ROOT = Path(__file__).resolve().parents[2]
# The loader and the codec it calls are the ONE place allowed to know the file's shape.
EXEMPT = {"api_keys.rs", "dpapi.rs"}

# Reading the file at all, in a file that also names secrets.env, is the signature worth catching.
READS_FILE = re.compile(r"read_to_string\s*\(|File::open\s*\(|fs::read\s*\(")
NAMES_SECRETS = re.compile(r"secrets\.env|SECRETS_FILE")


def test_no_rust_file_parses_secrets_env_itself() -> None:
    """Flag a file READ that is actually reading the secrets file.

    Proximity, not mere mention: pipeline.rs and settings.rs both name secrets.env in prose while
    reading transcripts and settings.json, and flagging those would be noise that trains people to
    ignore this gate. A read within a few lines of the path being built is the real signature.
    """
    offenders: list[str] = []
    for path in sorted(SRC.rglob("*.rs")):
        if path.name in EXEMPT or "target" in path.parts:
            continue
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        secret_lines = [i for i, line in enumerate(lines) if NAMES_SECRETS.search(line)]
        if not secret_lines:
            continue
        for number, line in enumerate(lines, start=1):
            if not READS_FILE.search(line):
                continue
            if any(abs((number - 1) - i) <= 6 for i in secret_lines):
                offenders.append(f"{path.relative_to(SRC)}:{number}: {line.strip()[:100]}")

    if offenders:
        listed = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(
            "these read secrets.env directly instead of through ApiKeys::load, so they will hand a "
            "dpapi:<base64> ciphertext to a provider as if it were a key:\n" + listed
        )


def test_the_live_key_test_uses_the_production_loader() -> None:
    """The specific regression: openrouter_live.rs must call ApiKeys, not split lines."""
    path = SRC / "tests" / "openrouter_live.rs"
    if not path.is_file():
        raise AssertionError(f"{path} is missing — this gate would pass vacuously")
    text = path.read_text(encoding="utf-8")
    assert "ApiKeys::load" in text, (
        "openrouter_live.rs must read its key through ApiKeys::load; hand-parsing secrets.env sends "
        "the encrypted blob as a bearer token"
    )
    assert "strip_prefix(&format!(\"{name}=\"))" not in text, "the hand-parser is back"


PROOF_REGISTERED_CONFIG = REPO_ROOT / "cortex-speech-app" / "src-tauri" / ".cargo" / "config.toml"
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "ci.yml"
LOADER = SRC / "src" / "api_keys.rs"


def test_no_cargo_config_outside_the_registered_one_and_ci_blanks_cloud_keys() -> None:
    """Test isolation from ambient keys must never come from a cargo config outside src-tauri.

    prepare_owner_proof_inputs._require_exact_cargo_configuration walks src-tauri and EVERY parent
    and refuses the helper build when any .cargo/config(.toml) other than the registered one exists;
    the registered file's sha256 is bound into owner_proof_input_contract.v1.json. Measured
    2026-09-02: a root .cargo/config.toml (PR #74) made proof preparation refuse on the release
    workstation for five hours while every CI run stayed green. Isolation therefore lives in ci.yml
    (both names blank at the top level; a blank value counts as unset in the loader) and in the
    library test binary never reading the process environment.
    """
    strays = [
        str(path.relative_to(REPO_ROOT))
        for base in (REPO_ROOT, REPO_ROOT / "cortex-speech-app")
        for name in ("config", "config.toml")
        for path in [base / ".cargo" / name]
        if path.exists()
    ]
    assert not strays, (
        "a cargo config outside src-tauri/.cargo makes the owner-proof helper build refuse "
        "('an alternate Cargo configuration could influence the owner-proof helper build'):\n"
        + "\n".join(f"- {s}" for s in strays)
    )
    assert PROOF_REGISTERED_CONFIG.is_file(), "the registered src-tauri/.cargo/config.toml is missing"
    workflow = WORKFLOW.read_text(encoding="utf-8")
    top_env = workflow.split("\njobs:", 1)[0]
    for name in ("GEMINI_API_KEY", "OPENROUTER_API_KEY"):
        assert f'{name}: ""' in top_env, f"ci.yml must blank {name} in its top-level env so no runner secret reaches a test binary"
    loader = LOADER.read_text(encoding="utf-8")
    assert "#[cfg(not(test))]\n        overlay_environment(&mut map, |name| std::env::var(name).ok());" in loader, (
        "ApiKeys::load must skip the process-environment overlay under cfg(test); the library test "
        "binary must never see an exported key"
    )


def main() -> None:
    test_no_rust_file_parses_secrets_env_itself()
    test_the_live_key_test_uses_the_production_loader()
    test_no_cargo_config_outside_the_registered_one_and_ci_blanks_cloud_keys()
    print("credential loader policy passed")


if __name__ == "__main__":
    main()
