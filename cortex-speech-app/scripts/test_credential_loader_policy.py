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
would hand it to every test binary. The root `.cargo/config.toml` blanks both key names for every
cargo-launched process, and a blank variable counts as unset in the loader (it neither overrides
nor clears a stored key); the third test pins the config, `api_keys::tests` pins the rule.
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


def test_cargo_blanks_cloud_keys_for_every_test_binary() -> None:
    """An exported key must never reach a test binary: red suite at best, a real upload at worst.

    `ApiKeys::load` overlays the environment over secrets.env. cargo applies `[env]` to every process
    it launches, test binaries included, and a BLANK value counts as unset in the loader. The file
    must be at the repository root: cargo discovers config from the current directory upward, and
    cargo is run from the root Makefile, from cortex-speech-app/ (CI) and from src-tauri/.
    """
    path = REPO_ROOT / ".cargo" / "config.toml"
    if not path.is_file():
        raise AssertionError(f"{path} is missing — every cargo-launched test binary sees the ambient keys")
    text = path.read_text(encoding="utf-8")
    assert "[env]" in text, "the [env] table is gone"
    for name in ("GEMINI_API_KEY", "OPENROUTER_API_KEY"):
        assert f'{name} = {{ value = "", force = true }}' in text, (
            f"{name} must be blanked with force = true; without force an exported value wins, and a "
            "non-empty value would be read as a key"
        )


def main() -> None:
    test_no_rust_file_parses_secrets_env_itself()
    test_the_live_key_test_uses_the_production_loader()
    test_cargo_blanks_cloud_keys_for_every_test_binary()
    print("credential loader policy passed")


if __name__ == "__main__":
    main()
