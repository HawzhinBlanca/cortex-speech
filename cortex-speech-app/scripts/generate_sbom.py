"""Emit a deterministic CycloneDX SBOM for both dependency graphs from their LOCKFILES.

Scope, stated honestly because an SBOM that overclaims is worse than none:

- Source of truth is `package-lock.json` and `src-tauri/Cargo.lock`. Those are the resolved graphs
  that actually get built, so this reports what ships, not what `package.json` asks for.
- Hashes are the ones the lockfiles already carry: Cargo's SHA-256 `checksum`, npm's `integrity`
  (decoded from base64 to hex, algorithm preserved). No hash is invented; a package without one is
  emitted without a `hashes` array rather than with a fabricated entry.
- Licences are NOT declared. Neither lockfile records them reliably, and guessing a licence in a
  compliance artifact is precisely the kind of confident-but-unverified claim this repo forbids.
  Licence review stays with `cargo deny check licenses`, which the release workflow already runs.
- Workspace-local and root packages carry no `source`/`resolved` and are skipped: they are the
  subject of the SBOM, not dependencies of it.

Why not `npm sbom`: measured 2026-08-06, it refuses on this project with ESBOMPROBLEMS because a
clean `npm ci` from the committed lockfile yields a tree npm itself calls invalid
(`picomatch@2.3.2` where `svelte-check`'s `fdir@6.5.0` requires `^3 || ^4`). Neither `npm install`
nor a targeted `overrides` entry resolves it. Reading the lockfiles directly is immune to that
hoisting defect, but the defect is real and tracked separately.

Deterministic on purpose: components are sorted by purl and no timestamp is embedded, so the same
inputs always produce byte-identical output and the SBOM can be checksummed and attested alongside
the other release artifacts.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
from pathlib import Path
from typing import Any


SPEC_VERSION = "1.5"
_CARGO_PACKAGE = re.compile(r"^\[\[package\]\]\s*$")
_CARGO_FIELD = re.compile(r'^(name|version|source|checksum)\s*=\s*"(.*)"\s*$')


def _purl_npm(name: str, version: str) -> str:
    # A scoped name (@scope/pkg) must percent-encode its leading @ per the purl spec.
    if name.startswith("@"):
        scope, _, bare = name.partition("/")
        return f"pkg:npm/%40{scope[1:]}/{bare}@{version}"
    return f"pkg:npm/{name}@{version}"


def _hashes_from_integrity(integrity: str | None) -> list[dict[str, str]]:
    """npm records `sha512-<base64>`; CycloneDX wants the digest as hex."""
    if not integrity or "-" not in integrity:
        return []
    algorithm, _, encoded = integrity.partition("-")
    alg = {"sha512": "SHA-512", "sha384": "SHA-384", "sha256": "SHA-256", "sha1": "SHA-1"}.get(algorithm.lower())
    if alg is None:
        return []
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error):
        return []
    return [{"alg": alg, "content": raw.hex()}]


def npm_components(lock: dict[str, Any]) -> list[dict[str, Any]]:
    components = []
    for path, entry in lock.get("packages", {}).items():
        if not path:
            continue  # the root project itself, not a dependency
        if entry.get("link"):
            continue  # a symlink to a workspace sibling, not a fetched artifact
        name = entry.get("name") or path.split("node_modules/")[-1]
        version = entry.get("version")
        if not name or not version:
            continue
        component: dict[str, Any] = {
            "type": "library",
            "name": name,
            "version": version,
            "purl": _purl_npm(name, version),
        }
        hashes = _hashes_from_integrity(entry.get("integrity"))
        if hashes:
            component["hashes"] = hashes
        components.append(component)
    return components


def cargo_components(lock_text: str) -> list[dict[str, Any]]:
    components = []
    current: dict[str, str] = {}

    def flush() -> None:
        # No `source` means a workspace-local crate (the app itself), not a fetched dependency.
        if current.get("name") and current.get("version") and current.get("source"):
            component: dict[str, Any] = {
                "type": "library",
                "name": current["name"],
                "version": current["version"],
                "purl": f"pkg:cargo/{current['name']}@{current['version']}",
            }
            if current.get("checksum"):
                component["hashes"] = [{"alg": "SHA-256", "content": current["checksum"]}]
            components.append(component)

    for line in lock_text.splitlines():
        if _CARGO_PACKAGE.match(line):
            flush()
            current = {}
            continue
        match = _CARGO_FIELD.match(line)
        if match:
            current[match.group(1)] = match.group(2)
    flush()
    return components


def build_sbom(npm_lock_path: Path, cargo_lock_path: Path, name: str, version: str) -> dict[str, Any]:
    components = []
    if npm_lock_path.is_file():
        components += npm_components(json.loads(npm_lock_path.read_text(encoding="utf-8")))
    if cargo_lock_path.is_file():
        components += cargo_components(cargo_lock_path.read_text(encoding="utf-8"))
    if not components:
        raise ValueError(f"no dependencies found in {npm_lock_path} or {cargo_lock_path}")

    # Sort AND de-duplicate by purl: the same crate can appear under several npm paths.
    unique: dict[str, dict[str, Any]] = {}
    for component in components:
        unique.setdefault(component["purl"], component)

    return {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "version": 1,
        "metadata": {"component": {"type": "application", "name": name, "version": version}},
        "components": [unique[purl] for purl in sorted(unique)],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app-root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--name", default="cortex-speech-app")
    parser.add_argument("--version", default="")
    args = parser.parse_args()

    version = args.version
    if not version:
        package_json = args.app_root / "package.json"
        version = json.loads(package_json.read_text(encoding="utf-8")).get("version", "0.0.0")

    sbom = build_sbom(
        args.app_root / "package-lock.json",
        args.app_root / "src-tauri" / "Cargo.lock",
        args.name,
        version,
    )
    args.output.write_text(json.dumps(sbom, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {args.output} ({len(sbom['components'])} components)")


if __name__ == "__main__":
    main()
