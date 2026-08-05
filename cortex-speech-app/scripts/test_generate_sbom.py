"""Regressions for generate_sbom.py, plus one run against the REAL lockfiles.

The fixtures pin the behaviour that makes the artifact trustworthy: no invented hashes, no invented
licences, deterministic ordering, and both ecosystems actually present.
"""

from __future__ import annotations

import base64
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from generate_sbom import build_sbom, cargo_components, npm_components  # noqa: E402


APP_ROOT = Path(__file__).resolve().parent.parent


def test_npm_integrity_becomes_a_real_hex_digest() -> None:
    raw = hashlib.sha512(b"payload").digest()
    lock = {
        "packages": {
            "": {"name": "root", "version": "1.0.0"},
            "node_modules/left-pad": {
                "version": "1.3.0",
                "integrity": "sha512-" + base64.b64encode(raw).decode("ascii"),
            },
        }
    }
    [component] = npm_components(lock)
    assert component["purl"] == "pkg:npm/left-pad@1.3.0"
    assert component["hashes"] == [{"alg": "SHA-512", "content": raw.hex()}]


def test_a_package_without_integrity_gets_no_hashes_key_rather_than_a_fake_one() -> None:
    lock = {"packages": {"node_modules/no-integrity": {"version": "0.1.0"}}}
    [component] = npm_components(lock)
    assert "hashes" not in component, "an absent digest must be absent, never fabricated"


def test_scoped_npm_names_percent_encode_the_at_sign() -> None:
    lock = {"packages": {"node_modules/@scope/pkg": {"name": "@scope/pkg", "version": "2.0.0"}}}
    [component] = npm_components(lock)
    assert component["purl"] == "pkg:npm/%40scope/pkg@2.0.0"


def test_cargo_workspace_local_crates_are_not_reported_as_dependencies() -> None:
    lock_text = """
[[package]]
name = "cortex-speech-app"
version = "2.1.0"

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc123"
"""
    components = cargo_components(lock_text)
    assert [c["name"] for c in components] == ["serde"], "the app itself is the SBOM subject, not a component"
    assert components[0]["hashes"] == [{"alg": "SHA-256", "content": "abc123"}]


def test_no_component_declares_a_licence() -> None:
    # Neither lockfile records licences reliably. Guessing one in a compliance artifact would be a
    # confident unverified claim; cargo-deny owns licence review.
    sbom = build_sbom(APP_ROOT / "package-lock.json", APP_ROOT / "src-tauri" / "Cargo.lock", "x", "1")
    assert not any("licenses" in c for c in sbom["components"])


def test_output_is_deterministic_and_covers_both_ecosystems() -> None:
    args = (APP_ROOT / "package-lock.json", APP_ROOT / "src-tauri" / "Cargo.lock", "cortex-speech-app", "2.1.0")
    first = build_sbom(*args)
    second = build_sbom(*args)
    assert json.dumps(first, sort_keys=True) == json.dumps(second, sort_keys=True)

    purls = [c["purl"] for c in first["components"]]
    assert purls == sorted(purls), "components must be sorted so the artifact is byte-stable"
    assert len(purls) == len(set(purls)), "a purl must not appear twice"
    assert any(p.startswith("pkg:npm/") for p in purls), "npm graph missing"
    assert any(p.startswith("pkg:cargo/") for p in purls), "cargo graph missing"
    assert first["bomFormat"] == "CycloneDX" and first["specVersion"] == "1.5"


def test_cli_writes_a_parseable_sbom_from_the_real_lockfiles() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "sbom.cdx.json"
        subprocess.run(
            [sys.executable, str(APP_ROOT / "scripts" / "generate_sbom.py"), "--output", str(out)],
            check=True,
            capture_output=True,
        )
        sbom = json.loads(out.read_text(encoding="utf-8"))
        assert len(sbom["components"]) > 100, f"suspiciously small SBOM: {len(sbom['components'])}"


def main() -> None:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok   {name}")
    print("sbom generator policy passed")


if __name__ == "__main__":
    main()
