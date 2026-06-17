#!/usr/bin/env python3
"""CORTEX 10/10 verification gates.

Self-locating: all paths resolve relative to the repository root (the parent of
this script's directory), so the gate runs identically from any working
directory and from CI (`python "$GITHUB_WORKSPACE/scripts/verify_10.py"`).

Gates
-----
1. Manifest version + license alignment (package.json / tauri.conf.json / Cargo.toml).
2. Required repository assets present.
3. Provenance-ledger structural validity (JSON Schema via `jsonschema` when
   installed; otherwise a dependency-free built-in validator).
4. License-compatibility / redistribution gate — FAILS THE BUILD when a corpus
   that is share-alike-contaminating, NonCommercial, or marked no-redistribution
   is declared for redistribution, or when the declared export license is
   incompatible with a share-alike corpus in the redistribute set.

This is the "gated, not asserted" enforcement the roadmap (M0/M1) requires;
green here means the policy actually holds, not merely that files are present.
"""
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# License the project publishes its *redistributable* dataset bundles under.
# Changing this is a deliberate, reviewed act: it governs the contamination gate.
EXPORT_LICENSE = "CC-BY-4.0"

# SPDX ids that are share-alike (copyleft): pulling any into a redistributed
# bundle forces the whole bundle to the same share-alike license.
SHARE_ALIKE_LICENSES = {"CC-BY-SA-4.0", "CC-BY-NC-SA-4.0", "GPL-3.0-only", "GPL-2.0-only"}

LEDGER_REQUIRED_KEYS = {
    "corpus": str,
    "sourceUrl": str,
    "spdxLicense": str,
    "shareAlike": bool,
    "attributionString": str,
    "consentBasis": str,
    "redistributionRights": str,
    "takedownContact": str,
    "datasetUsage": str,
}
DATASET_USAGE_VALUES = {"redistribute", "train_only", "reference_only", "excluded"}
REDIST_RIGHTS_VALUES = {
    "redistributable_with_attribution",
    "share_alike_contaminating",
    "train_only_no_redist",
    "permissive_public_domain",
}


def load_json(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def rel(path):
    """Resolve a repo-relative path against the detected repository root."""
    return REPO_ROOT / path


def check_manifests():
    print("==> Checking manifest version and license alignment...")
    pkg_path = rel("cortex-speech-app/package.json")
    tauri_path = rel("cortex-speech-app/src-tauri/tauri.conf.json")
    cargo_path = rel("cortex-speech-app/src-tauri/Cargo.toml")
    for p in (pkg_path, tauri_path, cargo_path):
        if not p.exists():
            print(f"  [ERR] {p} not found.")
            return False

    pkg = load_json(pkg_path)
    pkg_ver, pkg_license = pkg.get("version"), pkg.get("license")
    tauri_ver = load_json(tauri_path).get("version")

    cargo_ver = cargo_license = None
    content = cargo_path.read_text(encoding="utf-8")
    ver_match = re.search(r'^version\s*=\s*"([^"]+)"', content, re.MULTILINE)
    lic_match = re.search(r'^license\s*=\s*"([^"]+)"', content, re.MULTILINE)
    if ver_match:
        cargo_ver = ver_match.group(1)
    if lic_match:
        cargo_license = lic_match.group(1)

    print(f"  package.json:    version={pkg_ver}, license={pkg_license}")
    print(f"  tauri.conf.json: version={tauri_ver}")
    print(f"  Cargo.toml:      version={cargo_ver}, license={cargo_license}")

    ok = True
    if not (pkg_ver == tauri_ver == cargo_ver):
        print("  [ERR] Version mismatch across manifests!")
        ok = False
    if pkg_license != "Apache-2.0" or cargo_license != "Apache-2.0":
        print("  [ERR] License mismatch or not Apache-2.0!")
        ok = False
    return ok


def check_required_files():
    print("==> Checking required repository assets...")
    required = [
        "LICENSE",
        "NOTICE",
        "DATA_GOVERNANCE.md",
        "AGENT_CHARTER.md",
        "docs/ROADMAP_TO_10.md",
        "docs/RESEARCH_SOTA_2026.md",
        "docs/provenance_ledger.json",
        "docs/provenance_ledger.schema.json",
    ]
    ok = True
    for filepath in required:
        if rel(filepath).exists():
            print(f"  [OK]  Found {filepath}")
        else:
            print(f"  [ERR] Missing {filepath}")
            ok = False
    return ok


def _validate_ledger_builtin(ledger):
    errs = []
    if not isinstance(ledger, list) or not ledger:
        return ["ledger must be a non-empty array"]
    for i, row in enumerate(ledger):
        name = row.get("corpus", f"#{i}") if isinstance(row, dict) else f"#{i}"
        if not isinstance(row, dict):
            errs.append(f"row {name} is not an object")
            continue
        for key, typ in LEDGER_REQUIRED_KEYS.items():
            if key not in row:
                errs.append(f"corpus '{name}' missing key '{key}'")
            elif not isinstance(row[key], typ):
                errs.append(f"corpus '{name}' key '{key}' must be {typ.__name__}")
        usage = row.get("datasetUsage")
        if usage is not None and usage not in DATASET_USAGE_VALUES:
            errs.append(f"corpus '{name}' datasetUsage '{usage}' not in {sorted(DATASET_USAGE_VALUES)}")
        rights = row.get("redistributionRights")
        if rights is not None and rights not in REDIST_RIGHTS_VALUES:
            errs.append(f"corpus '{name}' redistributionRights '{rights}' not in {sorted(REDIST_RIGHTS_VALUES)}")
    return errs


def check_provenance_ledger():
    print("==> Checking provenance ledger schema integrity...")
    ledger_path = rel("docs/provenance_ledger.json")
    if not ledger_path.exists():
        print("  [ERR] docs/provenance_ledger.json not found.")
        return False
    try:
        ledger = load_json(ledger_path)
    except Exception as e:  # noqa: BLE001 - report any parse failure as a red gate
        print(f"  [ERR] parsing JSON: {e}")
        return False

    schema_path = rel("docs/provenance_ledger.schema.json")
    used = "built-in"
    errs = []
    try:
        import jsonschema  # type: ignore

        if schema_path.exists():
            schema = load_json(schema_path)
            validator = jsonschema.Draft202012Validator(schema)
            errs = [
                f"{'/'.join(map(str, e.path)) or '<root>'}: {e.message}"
                for e in validator.iter_errors(ledger)
            ]
            used = "jsonschema"
        else:
            errs = _validate_ledger_builtin(ledger)
    except ImportError:
        errs = _validate_ledger_builtin(ledger)

    if errs:
        print(f"  [ERR] ledger failed {used} validation:")
        for e in errs:
            print(f"        - {e}")
        return False

    for row in ledger:
        print(
            f"  [OK]  Corpus '{row.get('corpus')}' verified "
            f"({row.get('spdxLicense')}, usage={row.get('datasetUsage')}) [{used}]"
        )
    return True


def _is_noncommercial(spdx):
    spdx = spdx or ""
    return "-NC-" in spdx or spdx.endswith("-NC")


def check_license_compatibility():
    print("==> Checking dataset license-compatibility / redistribution gate...")
    ledger_path = rel("docs/provenance_ledger.json")
    if not ledger_path.exists():
        print("  [ERR] ledger not found.")
        return False
    ledger = load_json(ledger_path)
    redistribute = [r for r in ledger if r.get("datasetUsage") == "redistribute"]
    ok = True

    # Rule 1 — a no-redistribution corpus must never be in the redistribute set.
    for r in ledger:
        if r.get("redistributionRights") == "train_only_no_redist" and r.get("datasetUsage") == "redistribute":
            print(
                f"  [ERR] '{r.get('corpus')}' is no-redistribution "
                f"({r.get('spdxLicense')}) but datasetUsage=redistribute"
            )
            ok = False

    # Rule 2 — a share-alike / contaminating corpus in the redistribute set forces
    # the whole export to a share-alike license; fail if EXPORT_LICENSE is permissive.
    for r in redistribute:
        contaminating = (
            r.get("shareAlike") is True
            or r.get("redistributionRights") == "share_alike_contaminating"
            or r.get("spdxLicense") in SHARE_ALIKE_LICENSES
        )
        if contaminating and EXPORT_LICENSE not in SHARE_ALIKE_LICENSES:
            print(
                f"  [ERR] share-alike corpus '{r.get('corpus')}' ({r.get('spdxLicense')}) "
                f"is in the redistribute set, but EXPORT_LICENSE={EXPORT_LICENSE} is not "
                f"share-alike (license contamination)"
            )
            ok = False

    # Rule 3 — NonCommercial corpora must not be redistributed.
    for r in redistribute:
        if _is_noncommercial(r.get("spdxLicense")):
            print(
                f"  [ERR] NonCommercial corpus '{r.get('corpus')}' ({r.get('spdxLicense')}) "
                f"must not be in the redistribute set"
            )
            ok = False

    if ok:
        names = ", ".join(r.get("corpus") for r in redistribute) or "(none)"
        print(f"  [OK]  redistribute set [{names}] compatible with export license {EXPORT_LICENSE}")
    return ok


def main():
    print("==================================================")
    print("          CORTEX 10/10 VERIFICATION GATES         ")
    print("==================================================")
    print(f"(repo root: {REPO_ROOT})")

    gates = [
        check_manifests(),
        check_required_files(),
        check_provenance_ledger(),
        check_license_compatibility(),
    ]

    print("--------------------------------------------------")
    if all(gates):
        print("CORTEX 10/10: ALL GATES GREEN")
        sys.exit(0)
    print("CORTEX VERIFICATION FAILED: RED GATES PRESENT")
    sys.exit(1)


if __name__ == "__main__":
    main()
