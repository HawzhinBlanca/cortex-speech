#!/usr/bin/env python3
"""CORTEX verify-10 — the personal-use full-charter gate aggregator.

Self-locating: all paths resolve relative to the repository root (the parent of
this script's directory), so the gate runs identically from any working
directory and from CI (`python "$GITHUB_WORKSPACE/scripts/verify_10.py" --static`).

Modes
-----
  (default)  owner-product aggregator: Tier 0 static governance, Tier 1 CI-equivalent
             code gates, Tier 2 real-binary gates, Tier 3 deep proof legs
             (env-gated). Prints every owner-descoped and owner-gated charter
             leg explicitly — skipped legs are REPORTED, never silently dropped.
  --static   exactly the historical four governance gates (CI contract:
             ci.yml `governance-gate` and release.yml call this).
  --quick    Tiers 0-1 only. Tier-2/3 kept gates are counted NOT-RUN-QUICK, so
             the verdict is at best INCOMPLETE (exit 2) — never a ship verdict.

Verdict contract (exactly one final line):
  RED (exit 1)         — a kept gate failed.
  INCOMPLETE (exit 2)  — no failures, but a kept gate could not run
                         (missing env or not yet built). Green cannot be claimed.
  CORTEX PRODUCT 10/10 — OWNER WORKSTATION (exit 0) — every selected gate and
                         mandatory evidence class passed for the exact bound release.
  CORTEX PRODUCT 10/10 — WINDOWS 11 (exit 0) — the owner/core contract plus
                         signed Windows distribution, VM, accessibility and field proof passed.
  CORTEX 10/10: ALL GATES GREEN (exit 0) — only possible when nothing is
                         descoped or owner-gated: per SHIP_FINAL_PLAN #58 this
                         can only ever happen after the P7 re-audit.

Evidence classes without verifier-owned validators remain explicitly pending. Generic self-authored
pass documents are not accepted, and Windows signing/distribution requirements are never descoped.
"""
import argparse
import contextlib
import ctypes
import hashlib
import importlib.util
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import asdict, dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable, Iterator, Sequence

_VERIFY_SCRIPT_DIR = str(Path(__file__).resolve().parent)
if _VERIFY_SCRIPT_DIR not in sys.path:
    sys.path.insert(0, _VERIFY_SCRIPT_DIR)

try:
    from verify10_supervisor import (
        EvidenceError,
        EvidenceJournal,
        LeaseError,
        LeaseManager,
        acquired_lease,
        atomic_write_bytes,
        atomic_write_json,
        publish_validated_json,
        sha256_file,
        spawn_isolated,
        terminate_isolated,
        utc_now,
        wait_isolated,
    )
except ModuleNotFoundError:  # Imported as ``scripts.verify_10`` by policy tests.
    from scripts.verify10_supervisor import (
        EvidenceError,
        EvidenceJournal,
        LeaseError,
        LeaseManager,
        acquired_lease,
        atomic_write_bytes,
        atomic_write_json,
        publish_validated_json,
        sha256_file,
        spawn_isolated,
        terminate_isolated,
        utc_now,
        wait_isolated,
    )

try:
    from windows_release_bundle import ReleaseBundleError, validate_bundle as validate_windows_release_bundle
except ModuleNotFoundError:  # Imported as ``scripts.verify_10`` by policy tests.
    from scripts.windows_release_bundle import (
        ReleaseBundleError,
        validate_bundle as validate_windows_release_bundle,
    )

REPO_ROOT = Path(__file__).resolve().parent.parent
APP = REPO_ROOT / "cortex-speech-app"
SRC_TAURI = APP / "src-tauri"
MANIFEST = SRC_TAURI / "Cargo.toml"
EXE = SRC_TAURI / "target" / "release" / "cortex-speech-app.exe"
ACTIVE_RELEASE_POINTER = "active-private-production-release.json"
_RUNTIME_EXE_CONFIGURED = False
_RUNTIME_EXE_ERROR = None
_WINDOWS_RELEASE_AUTHORITY: dict[str, object] | None = None


def validate_active_release_runtime(manifest, release_root):
    """Return the exact immutable app binary, or fail closed on pointer/hash/SHA drift."""
    if not isinstance(manifest, dict) or manifest.get("schema") != 1:
        raise ValueError("active release pointer is not a schema-1 object")
    directory_value = manifest.get("directory")
    exe_value = manifest.get("appExe")
    expected_hash = manifest.get("appSha256")
    expected_git_sha = manifest.get("appGitSha")
    if not all(isinstance(value, str) and value for value in (directory_value, exe_value, expected_hash, expected_git_sha)):
        raise ValueError("active release pointer lacks app path/hash/git authority")
    if not re.fullmatch(r"[0-9a-f]{64}", expected_hash):
        raise ValueError("active release app hash is not canonical SHA-256")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_git_sha):
        raise ValueError("active release app git SHA is not canonical")

    root = Path(release_root).resolve(strict=True)
    directory = Path(directory_value).resolve(strict=True)
    exe = Path(exe_value).resolve(strict=True)
    if directory.parent != root or exe.parent != directory or exe.name.casefold() != "cortex-speech-app.exe":
        raise ValueError("active release app path escapes its immutable release directory")
    blob = exe.read_bytes()
    actual_hash = hashlib.sha256(blob).hexdigest()
    if actual_hash != expected_hash:
        raise ValueError(f"active release app hash drifted ({actual_hash[:12]}… != {expected_hash[:12]}…)")
    marker = re.search(rb"CORTEX_BUILD_SHA:([0-9a-fA-F]{7,40}|unknown)", blob)
    baked_sha = marker.group(1).decode("ascii").casefold() if marker else None
    if baked_sha is None or not (expected_git_sha.startswith(baked_sha) or baked_sha.startswith(expected_git_sha)):
        raise ValueError("active release app binary does not carry its manifest git SHA")
    return exe


def configure_runtime_exe():
    """Prefer an explicit diagnostic exe, then the validated immutable production release."""
    global _RUNTIME_EXE_CONFIGURED, _RUNTIME_EXE_ERROR
    if _RUNTIME_EXE_CONFIGURED:
        return
    _RUNTIME_EXE_CONFIGURED = True
    if os.environ.get("CORTEX_APP_EXE"):
        return
    appdata = os.environ.get("APPDATA")
    localappdata = os.environ.get("LOCALAPPDATA")
    if not appdata or not localappdata:
        return
    pointer = Path(appdata) / "cortex-speech" / ACTIVE_RELEASE_POINTER
    if not pointer.is_file():
        return
    try:
        manifest = json.loads(pointer.read_text(encoding="utf-8"))
        release_root = Path(localappdata) / "CortexSpeech" / "private-production-releases"
        os.environ["CORTEX_APP_EXE"] = str(validate_active_release_runtime(manifest, release_root))
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        _RUNTIME_EXE_ERROR = f"active immutable release pointer is invalid: {error}"


def runtime_exe():
    configure_runtime_exe()
    return Path(os.environ.get("CORTEX_APP_EXE", str(EXE)))

# A SEPARATE cargo target dir for the fault-drill binaries, and it is not a preference.
# `tauri_build`/`ort` copy `onnxruntime.dll` next to the built artifacts, and the RUNNING app holds
# that dll open — so `cargo build --bin durability_writer` against the normal target dir dies with
# "The process cannot access the file because it is being used by another process. (os error 32)".
# Measured on 2026-08-02 with the app up: exit 101. The app is up during every real sweep (it is the
# machine's normal state, and other legs depend on it), so a drill leg building into `target/` would
# fail for a reason that has nothing to do with what it tests. A sibling dir has its own copy of the
# dll that nothing holds. Inside `target/`, which is already gitignored. First run pays a full
# dependency build; after that it is cached like any other target dir.
DRILL_TARGET = SRC_TAURI / "target" / "drills"
DRILL_BIN = DRILL_TARGET / "release"

# real_audio.rs's helpers return an EMPTY set when this is unset (discover_real_audio_files ->
# Vec::new(), and one test returns early printing "set CORTEX_REAL_AUDIO_DIR"), so the
# ignored-real-model leg reported "21 passed" while TWELVE of those tests asserted nothing. Measured
# 2026-08-02: pointing it at the committed fixtures drops that to nine, turning decode-any-format,
# single-file decode and the pipeline import test into real assertions. The rest need formats the repo
# does not carry (flac/mov/mp4, the gold podcast) or their own env vars, and stay honestly skipped.
# setdefault, not assignment: an owner with a richer audio directory keeps theirs.
os.environ.setdefault("CORTEX_REAL_AUDIO_DIR", str(SRC_TAURI / "tests" / "fixtures"))

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

# ---------------------------------------------------------------------------
# Tier 0 — static governance checks (in-process; == --static plus extensions)
# ---------------------------------------------------------------------------


def load_json(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def rel(path):
    """Resolve a repo-relative path against the detected repository root."""
    return REPO_ROOT / path


def _changelog_top_version():
    """First released version heading in the canonical CHANGELOG ('Unreleased' skipped)."""
    text = rel("cortex-speech-app/CHANGELOG.md").read_text(encoding="utf-8")
    for m in re.finditer(r"^## \[([^\]]+)\]", text, re.MULTILINE):
        if m.group(1).lower() != "unreleased":
            return m.group(1)
    return None


def check_manifests():
    print("==> Checking manifest version and license alignment...")
    pkg_path = rel("cortex-speech-app/package.json")
    tauri_path = rel("cortex-speech-app/src-tauri/tauri.conf.json")
    cargo_path = rel("cortex-speech-app/src-tauri/Cargo.toml")
    changelog_path = rel("cortex-speech-app/CHANGELOG.md")
    for p in (pkg_path, tauri_path, cargo_path, changelog_path):
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

    changelog_ver = _changelog_top_version()

    print(f"  package.json:    version={pkg_ver}, license={pkg_license}")
    print(f"  tauri.conf.json: version={tauri_ver}")
    print(f"  Cargo.toml:      version={cargo_ver}, license={cargo_license}")
    print(f"  CHANGELOG.md:    version={changelog_ver}")

    ok = True
    if not (pkg_ver == tauri_ver == cargo_ver):
        print("  [ERR] Version mismatch across manifests!")
        ok = False
    if changelog_ver != pkg_ver:
        print("  [ERR] Canonical CHANGELOG version does not byte-equal the manifests!")
        ok = False
    # PolyForm Noncommercial 1.0.0 (2026-07-14 relicense, owner decision): the app's own source went
    # from Apache-2.0 (freely commercially reusable, which is what let third parties embed it in their
    # own products) to a noncommercial-use license. Bundled THIRD-PARTY deps (Meta OmniASR, sherpa-onnx,
    # Silero VAD) keep their own Apache-2.0 terms unaffected — see NOTICE — this gate is only about the
    # project's own declared license.
    if pkg_license != "PolyForm-Noncommercial-1.0.0" or cargo_license != "PolyForm-Noncommercial-1.0.0":
        print("  [ERR] License mismatch or not PolyForm-Noncommercial-1.0.0!")
        ok = False
    return ok


def check_repo_integrity():
    """LICENSE is PolyForm Noncommercial 1.0.0 text, NOTICE names the project, Cargo repository URL is the real remote."""
    print("==> Checking LICENSE/NOTICE content and repository URL...")
    ok = True

    license_head = "\n".join(rel("LICENSE").read_text(encoding="utf-8").splitlines()[:5])
    if "PolyForm Noncommercial License 1.0.0" not in license_head:
        print("  [ERR] LICENSE does not begin with the PolyForm Noncommercial License text.")
        ok = False
    else:
        print("  [OK]  LICENSE is the PolyForm Noncommercial License text.")

    notice_head = rel("NOTICE").read_text(encoding="utf-8").splitlines()
    if not notice_head or "Cortex" not in notice_head[0]:
        print("  [ERR] NOTICE does not name the project on its first line.")
        ok = False
    else:
        print(f"  [OK]  NOTICE names the project: {notice_head[0]!r}")

    repo_match = re.search(
        r'^repository\s*=\s*"([^"]+)"', MANIFEST.read_text(encoding="utf-8"), re.MULTILINE
    )
    declared = repo_match.group(1) if repo_match else None
    try:
        remote = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            timeout=15,
        ).stdout.strip()
    except Exception:  # noqa: BLE001 - no git available: fall back to placeholder check
        remote = ""
    if remote:
        if declared and declared.rstrip("/") == remote.removesuffix(".git").rstrip("/"):
            print(f"  [OK]  Cargo.toml repository == origin remote ({declared})")
        else:
            print(f"  [ERR] Cargo.toml repository {declared!r} != origin remote {remote!r}")
            ok = False
    elif not declared or "github.com/cortex/kurdish-speech" in declared:
        print(f"  [ERR] Cargo.toml repository is a placeholder: {declared!r}")
        ok = False
    else:
        print(f"  [OK]  Cargo.toml repository set ({declared}); git remote unavailable to cross-check.")
    return ok


def check_clean_source_tree():
    """Certification is about one committed tree, never an unrecorded working-copy variant."""

    completed = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    if completed.returncode != 0:
        print("  [ERR] git status could not prove a clean source tree")
        return False
    dirty = [line for line in completed.stdout.splitlines() if line.strip()]
    if dirty:
        print(f"  [ERR] source tree has {len(dirty)} tracked/untracked change(s)")
        for line in dirty[:20]:
            print(f"    {line}")
        return False
    print("  [OK] source tree is clean and exactly represented by HEAD")
    return True


def check_branch_protection():
    """`main` is protected on the REMOTE, verified against GitHub — not taken on trust.

    This was OWNER_GATED as "item 49: repo-admin clicks" — an item whose only evidence was that
    somebody said they had clicked. Protection can also be silently weakened later (a context renamed,
    admins exempted, force-push re-allowed) and nothing here would have noticed. It is an API call;
    there is no reason for it to be a manual claim.

    Anti-vacuity: an empty required-contexts list FAILS. A branch that is "protected" while requiring
    no checks is not protected, and answering 200 is not the same as being safe. Every required
    context must also still name a real job in .github/workflows, so a renamed job that quietly stops
    gating merges is caught rather than sitting there as a permanently-pending phantom.
    """
    print("==> Checking branch protection on origin/main (GitHub API)...")
    ok = True
    try:
        raw = subprocess.run(
            ["gh", "api", "repos/{owner}/{repo}/branches/main/protection"],
            capture_output=True, text=True, cwd=REPO_ROOT, timeout=60,
        )
    except Exception as exc:  # network/gh blew up mid-call
        print(f"  [ERR] could not query branch protection: {exc}")
        return False
    if raw.returncode != 0:
        print(f"  [ERR] gh api failed: {(raw.stderr or raw.stdout).strip()[:200]}")
        return False
    try:
        data = json.loads(raw.stdout)
    except json.JSONDecodeError as exc:
        print(f"  [ERR] branch protection response was not JSON: {exc}")
        return False

    checks = data.get("required_status_checks") or {}
    contexts = checks.get("contexts") or []
    if not contexts:
        print("  [ERR] main requires ZERO status checks — 'protected' but nothing gates a merge.")
        ok = False
    else:
        print(f"  [OK]  required status checks: {sorted(contexts)}")
        workflows = "\n".join(
            path.read_text(encoding="utf-8", errors="replace")
            for path in sorted((REPO_ROOT / ".github" / "workflows").glob("*.yml"))
        )
        for context in sorted(contexts):
            if context in workflows:
                continue
            print(f"  [ERR] required context {context!r} names no job in .github/workflows — "
                  "a merge would wait forever on a check nothing can report.")
            ok = False

    for label, value, want in (
        ("strict (branch must be up to date)", checks.get("strict"), True),
        ("enforce_admins", (data.get("enforce_admins") or {}).get("enabled"), True),
        ("required_linear_history", (data.get("required_linear_history") or {}).get("enabled"), True),
        ("allow_force_pushes", (data.get("allow_force_pushes") or {}).get("enabled"), False),
        ("allow_deletions", (data.get("allow_deletions") or {}).get("enabled"), False),
    ):
        if value is want:
            print(f"  [OK]  {label} = {value}")
        else:
            print(f"  [ERR] {label} = {value!r}, expected {want!r}")
            ok = False
    return ok


def _probe_branch_protection():
    """SKIP honestly without gh or without auth — never a silent pass."""
    if not shutil.which("gh"):
        return "gh CLI not installed (branch protection is a REMOTE fact; nothing local can prove it)"
    # timeout: `gh auth status` reaches the network. Unbounded, a wedged gh hangs the WHOLE sweep
    # here in the probe — before any gate has run — with nothing on stdout to say why.
    try:
        status = subprocess.run(["gh", "auth", "status"], capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return "`gh auth status` did not answer within 60s - cannot read branch protection"
    if status.returncode != 0:
        return "gh is not authenticated (`gh auth login`) - cannot read branch protection"
    return None


def check_required_files():
    print("==> Checking required repository assets...")
    required = [
        "LICENSE",
        "NOTICE",
        "SECURITY.md",
        ".github/CODEOWNERS",
        "DATA_GOVERNANCE.md",
        "AGENT_CHARTER.md",
        "docs/ROADMAP_TO_10.md",
        "docs/RESEARCH_SOTA_2026.md",
        "docs/provenance_ledger.json",
        "docs/provenance_ledger.schema.json",
        "cortex-speech-app/CHANGELOG.md",
        "cortex-speech-app/docs/MEASUREMENTS.md",
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


def static_main():
    """The historical governance gate — output contract preserved for CI."""
    print("==================================================")
    print("          CORTEX GOVERNANCE VERIFICATION          ")
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
        print("CORTEX GOVERNANCE: ALL GATES GREEN")
        sys.exit(0)
    print("CORTEX VERIFICATION FAILED: RED GATES PRESENT")
    sys.exit(1)


# ---------------------------------------------------------------------------
# Aggregator framework
# ---------------------------------------------------------------------------

PASS, PASS_AFTER_RETRY, FAIL, SKIP_ENV, NOT_BUILT = (
    "PASS",
    "PASS-AFTER-RETRY",
    "FAIL",
    "SKIP-ENV",
    "NOT-BUILT",
)
# --quick deliberately does not run tier-2/3 kept gates; they are counted with this status so the
# verdict is at best INCOMPLETE. Quick mode must never print the ship-ready GREEN line — that
# verdict was previously reachable ONLY in the least-verified mode (true-10 sweep 2026-07-11).
NOT_RUN_QUICK = "NOT-RUN-QUICK"


def _probe_deny():
    if shutil.which("cargo-deny"):
        return None
    return "cargo-deny not installed (cargo install cargo-deny)"


def _probe_exe():
    exe = runtime_exe()
    if _RUNTIME_EXE_ERROR:
        return _RUNTIME_EXE_ERROR
    if exe.exists():
        return None
    return f"release exe missing at {exe} - build a candidate or activate a validated immutable release"


def _probe_real_e2e():
    reason = _probe_exe()
    if reason:
        return reason
    # This used to skip whenever CORTEX_AUDIO was unset, which made the leg the registration below
    # calls "THE daily-use reliability gate" the easiest one in the suite to not run: a sweep came
    # back "22 PASS, 0 FAIL" with it reported SKIP-ENV. The harness now defaults to the committed
    # FLEURS ckb fixture, so the only honest reason left to skip is that fixture being absent.
    # CORTEX_AUDIO still overrides it, and the harness prints whichever path it used.
    if not os.environ.get("CORTEX_AUDIO") and not (
        SRC_TAURI / "tests" / "fixtures" / "fleurs_ckb_sample.wav"
    ).exists():
        return "committed audio fixture missing - set CORTEX_AUDIO=<absolute wav path> instead"
    return _probe_champion_7b()


def _probe_bench():
    if not (SRC_TAURI / "benches").is_dir():
        return "criterion bench targets missing"
    if not (APP / "docs" / "bench_baseline.json").exists():
        return "no committed baseline - run `python scripts/bench_gate.py --update --runs 3` with the app running"
    return None


def _probe_ipc_harness():
    """Shared executable/fixture probe for disposable-profile IPC harnesses.

    They now default to that fixture and run against a DISPOSABLE profile (e2e_profile.cjs), so the
    only generic reasons to skip are a missing binary or fixture — not a forgotten env var.
    """
    reason = _probe_exe()
    if reason:
        return reason
    if not (SRC_TAURI / "tests" / "fixtures" / "fleurs_ckb_sample.wav").exists():
        return "committed audio fixture missing"
    return None


def _probe_champion_ipc_harness():
    reason = _probe_ipc_harness()
    return reason or _probe_champion_7b()


def _probe_champion_7b():
    """The champion server lives in WSL, outside the tree, so its absence is machine state.

    Split out of `ignored-real-model` on 2026-08-17. `wsl_7b_preflight_passes_when_server_up` failed
    there because the server was down, which turned the whole leg RED — taking six real-model tests
    that had genuinely PASSED down with it and burying the sweep's actual failures under an
    environmental one. A leg that cannot run must say SKIP-ENV, and the other six must keep running.

    STRENGTHENED 2026-08-20 (external review: "the 7B gate checks only whether a port is open").
    A reachable port proves a listener, not the champion: this now speaks the protocol — sends
    {"op": "health"} and requires status=ready AND the exact deploymentSha256 the live
    champion.json pins. A wrong or half-loaded model on the right port is a FAILURE, not a pass.

    And it says FAILURE in the status, not only in the prose: the two verdicts that mean the WRONG
    THING IS ANSWERING (identity mismatch, a reply that is not the champion protocol) return
    (FAIL, reason). Reported as a bare reason they became SKIP-ENV — "environment not ready" — which
    is precisely the 494/494-wrong-engine signal this probe was strengthened to raise. Absence and
    BUSY stay SKIP-ENV: those really are machine state.
    """
    import json as _json
    import os as _os
    import socket

    port = int(_os.environ.get("CORTEX_7B_PORT", "8799"))
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2.0) as probe:
            probe.settimeout(8.0)
            probe.sendall(b'{"op": "health"}\n')
            buf = bytearray()
            while b"\n" not in buf and len(buf) < 1024 * 1024:
                chunk = probe.recv(65536)
                if not chunk:
                    break
                buf.extend(chunk)
    except OSError:
        return f"OmniASR-7B champion server not up on 127.0.0.1:{port} (`wsl python scripts/cortex_7b_server.py`)"
    try:
        reply = _json.loads(bytes(buf).split(b"\n", 1)[0].decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        return FAIL, f"7B server answered on {port} but its health reply is unparseable ({exc}) — NOT the champion protocol"
    if reply.get("code") == "BUSY":
        return "7B server is saturated and returned BUSY without identity — retry when health can prove the champion pin"
    if reply.get("status") != "ready":
        return f"7B server on {port} is not ready: {reply.get('error') or reply.get('status')!r}"
    appdata = _os.environ.get("APPDATA")
    pointer = Path(appdata) / "cortex-speech" / "champion.json" if appdata else None
    if pointer and pointer.is_file():
        try:
            pinned = _json.loads(pointer.read_text(encoding="utf-8"))["champions"]["omniasr-7b"]["deploymentSha256"]
        except (ValueError, KeyError, OSError) as exc:
            return f"live champion.json is unreadable ({exc}) — cannot verify the served identity"
        served = reply.get("deploymentSha256")
        if served != pinned:
            return (
                FAIL,
                f"7B server on {port} serves deployment {str(served)[:12]}… but the live champion pin is "
                f"{pinned[:12]}… — the WRONG MODEL is answering the champion port",
            )
    return None


def _probe_egress():
    reason = _probe_exe()
    if reason:
        return reason
    if sys.platform != "win32":
        return "egress probe samples Windows TCP (Get-NetTCPConnection); runs on the owner Windows rig"
    return None


# --- fuzz-smoke ------------------------------------------------------------------------
# windows-msvc CANNOT link cargo-fuzz at all: ASAN's dynamic-CRT model multiply-defines std::
# symbols against the static-MT sherpa-onnx prebuilt (LNK2005), and --sanitizer none strips the
# runtime providing libFuzzer's sancov section symbols (LNK2001 __stop___sancov_pcs);
# sherpa-onnx-sys ships no MD prebuilt. Measured 2026-07-11, still true.
#
# But WSL on the same machine is a real Linux toolchain, and there the ASAN + -fPIC static libs
# link fine (verified 2026-07-26: all 5 targets built and ran, ~2.7M execs, 0 crashes). So on
# Windows this gate runs the targets THROUGH WSL rather than declaring itself unrunnable. That is
# the gate genuinely executing on this rig — not a relaxation.
def _wsl_path(win_path):
    """C:\\x\\y -> /mnt/c/x/y (WSL's default drive mount)."""
    p = str(win_path).replace("\\", "/")
    if len(p) > 1 and p[1] == ":":
        return f"/mnt/{p[0].lower()}{p[2:]}"
    return p


def _wsl_fuzz_available():
    """True when WSL exists AND has cargo-fuzz + a nightly toolchain."""
    if not shutil.which("wsl"):
        return False
    r = subprocess.run(
        ["wsl", "--", "bash", "-lc", "command -v cargo-fuzz >/dev/null && cargo +nightly --version"],
        capture_output=True,
        text=True,
        timeout=180,
    )
    return r.returncode == 0


def _drill_cmd(bin_name: str, script: str, extra: str) -> str:
    """Build the drill's writer binary into DRILL_TARGET, then run the drill against it.

    The build is part of the leg deliberately. Requiring it to be pre-built would mean either a probe
    that SKIPS (turning a reliability gate into a no-op exactly when someone forgot) or a stale binary
    silently proving durability for code that is no longer shipped. Cargo is a no-op when it is current,
    so the cost after the first sweep is the drill itself.

    `--release`: these drills race a kill against real write throughput, and a debug writer is slow
    enough that the kill lands somewhere unrepresentative.
    """
    exe = DRILL_BIN / f"{bin_name}.exe" if sys.platform == "win32" else DRILL_BIN / bin_name
    build = f'cargo build --release --bin {bin_name} --manifest-path "{MANIFEST}" --target-dir "{DRILL_TARGET}"'
    run = f'"{sys.executable}" "{APP / "scripts" / script}" --exe "{exe}" {extra}'
    return f"{build} && {run}"


def _probe_fuzz():
    if sys.platform == "win32":
        if _wsl_fuzz_available():
            return None  # runnable via WSL — see _fn_fuzz_smoke
        return (
            "cargo-fuzz cannot link on windows-msvc (ASAN CRT vs static-MT sherpa). Install it in "
            "WSL to run this leg locally: wsl -- bash -lc 'rustup toolchain install nightly && "
            "cargo install cargo-fuzz' (plus libdbus-1-dev, libssl-dev and the Tauri Linux deps); "
            "otherwise it runs in Linux CI."
        )
    if not shutil.which("cargo-fuzz"):
        return "cargo-fuzz not installed (cargo install cargo-fuzz + nightly toolchain)"
    return None


def _wsl_fuzz_cache_key():
    """Stable per-checkout cache key; Cargo fingerprints the exact source/toolchain inside it."""
    checkout = os.path.normcase(str(SRC_TAURI.resolve())).encode("utf-8")
    return hashlib.sha256(checkout).hexdigest()[:16]


def _fuzz_cmd(argstr):
    """`cargo +nightly fuzz <argstr>` with one fast, content-verified WSL build cache."""
    args = shlex.split(argstr)
    if not args:
        raise ValueError("cargo fuzz command cannot be empty")
    if sys.platform == "win32":
        # Building ASAN artifacts under /mnt/c is both dramatically slower and prone to recompiling
        # the full Tauri dependency graph for every target. Keep only build artifacts on WSL's ext4
        # filesystem; source and corpora remain in the checkout. The path is per checkout, and Cargo's
        # own fingerprints still bind every artifact to the exact source, lockfile, flags and toolchain.
        cache_key = _wsl_fuzz_cache_key()
        command = " ".join(shlex.quote(part) for part in args)
        shell = (
            "set -euo pipefail; "
            f'cache_dir="${{XDG_CACHE_HOME:-$HOME/.cache}}/cortex-speech/fuzz/{cache_key}"; '
            'mkdir -p "$cache_dir"; export CARGO_TARGET_DIR="$cache_dir"; '
            f"cd {shlex.quote(_wsl_path(SRC_TAURI))}; exec cargo +nightly fuzz {command}"
        )
        # `--exec` is material: plain `--` lets WSL's default shell expand `$cache_dir` once before
        # bash receives this script, turning the fail-closed cache path into an empty string.
        return ["wsl", "--exec", "bash", "-lc", shell]
    return ["cargo", "+nightly", "fuzz", *args]


def _fuzz_run_cmd(target):
    """Run an already-built harness; WSL avoids cargo-fuzz's redundant second Cargo build."""
    if not re.fullmatch(r"[A-Za-z0-9_-]+", target):
        raise ValueError(f"unsafe cargo fuzz target name: {target!r}")
    if sys.platform != "win32":
        return _fuzz_cmd(f"run {shlex.quote(target)} -- -max_total_time=30")

    cache_key = _wsl_fuzz_cache_key()
    fuzz_dir = _wsl_path(SRC_TAURI / "fuzz")
    # cargo-fuzz's own exec_fuzz first builds, then launches `cargo run`, which can rebuild this
    # large Tauri library even when the harness was just built. We already built every target above,
    # so execute that exact ASAN binary directly. Preserve cargo-fuzz 0.13.2's runtime defaults:
    # its corpus seeding and detect_odr_violation=0 ASAN option. Runtime corpus/artifacts stay in
    # the Linux cache so a proof run cannot dirty the checkout or retrigger Tauri's build script.
    shell = (
        "set -euo pipefail; "
        f'cache_dir="${{XDG_CACHE_HOME:-$HOME/.cache}}/cortex-speech/fuzz/{cache_key}"; '
        f'fuzz_dir={shlex.quote(fuzz_dir)}; target={shlex.quote(target)}; '
        'binary="$cache_dir/x86_64-unknown-linux-gnu/release/$target"; '
        'source_corpus="$fuzz_dir/corpus/$target"; '
        'artifacts="$cache_dir/runtime-artifacts/$target"; '
        'corpus="$cache_dir/runtime-corpus/$target"; '
        'test -x "$binary"; mkdir -p "$artifacts" "$corpus"; '
        'if [[ -d "$source_corpus" ]]; then cp -a "$source_corpus/." "$corpus/"; fi; '
        'if [[ -n "${ASAN_OPTIONS:-}" ]]; then '
        'export ASAN_OPTIONS="${ASAN_OPTIONS}:detect_odr_violation=0"; '
        'else export ASAN_OPTIONS="detect_odr_violation=0"; fi; '
        'exec "$binary" -artifact_prefix="$artifacts/" -max_total_time=30 "$corpus"'
    )
    return ["wsl", "--exec", "bash", "-lc", shell]


def _fn_fuzz_smoke():
    """30s smoke per fuzz target; PASS only if EVERY target actually ran and was crash-free."""
    try:
        lst = subprocess.run(_fuzz_cmd("list"), capture_output=True, text=True, timeout=180)
    except subprocess.TimeoutExpired:
        print("  [ERR] cargo fuzz list timed out after 180s")
        return False
    targets = [t for t in lst.stdout.split() if t]
    # Fail LOUD on an empty target list. A run that enumerates nothing would otherwise sail
    # through the loop below and return True — a vacuous pass, which is exactly the class of
    # dishonesty this repo's charter forbids. (Hit for real on 2026-07-26 when a non-login shell
    # left cargo off PATH: 0 targets, 0 iterations, "all clean".)
    if lst.returncode != 0 or not targets:
        print("  [ERR] cargo fuzz list failed or found no targets - refusing to report a pass")
        return False
    print(f"  {len(targets)} targets: {', '.join(targets)}")

    # One Cargo invocation builds all harnesses and their shared dependency graph. Calling `run`
    # cold for each target separately caused repeated multi-hundred-crate ASAN builds on this rig.
    try:
        build = subprocess.run(_fuzz_cmd("build"), capture_output=True, text=True, timeout=3600)
    except subprocess.TimeoutExpired:
        print("  [ERR] cargo fuzz build timed out after 3600s")
        return False
    if build.returncode != 0:
        print("  [ERR] cargo fuzz build failed")
        for line in (build.stderr or build.stdout).splitlines()[-20:]:
            print(f"    {line}")
        return False

    for t in targets:
        try:
            r = subprocess.run(
                _fuzz_run_cmd(t),
                capture_output=True,
                text=True,
                timeout=300,
            )
        except subprocess.TimeoutExpired:
            print(f"  fuzz {t}: TIMEOUT after 300s")
            return False
        output = (r.stdout or "") + "\n" + (r.stderr or "")
        done = re.search(r"(?m)^#(\d+)\s+.*\bDONE\b", output)
        iterations = int(done.group(1)) if done else 0
        actually_ran = iterations > 0
        ok = r.returncode == 0 and actually_ran
        detail = f"ok ({iterations:,} iterations)" if ok else "CRASH/FAIL/NO-RUN-EVIDENCE"
        print(f"  fuzz {t}: {detail}")
        if not ok:
            for line in output.splitlines()[-20:]:
                print(f"    {line}")
            return False
    return True


PROFILE_OWNER = "owner-product"
PROFILE_WINDOWS = "windows-product"
PROFILE_MODEL = "model-evidence"
PROFILE_FULL = "full-charter"
PROFILES = frozenset({PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_MODEL, PROFILE_FULL})

PENDING_EXTERNAL = "PENDING_EXTERNAL"
EVIDENCE_VERIFIED = "VERIFIED"
EVIDENCE_FAILED = "FAILED_VALIDATION"


@dataclass(frozen=True)
class EvidenceClassSpec:
    """One mandatory proof class whose absence must remain release-blocking.

    These are deliberately *not* generic user-authored checkboxes.  Each class stays pending until a
    verifier-owned, class-specific validator exists and has emitted hash-bound artifacts.  Treating an
    arbitrary JSON document that says ``passed`` as proof would merely automate manual status editing.
    """

    id: str
    profiles: frozenset[str]
    description: str


_OWNER_EVIDENCE = frozenset({PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_FULL})
_WINDOWS_EVIDENCE = frozenset({PROFILE_WINDOWS, PROFILE_FULL})
_MODEL_EVIDENCE = frozenset({PROFILE_MODEL, PROFILE_FULL})

EVIDENCE_CLASSES: tuple[EvidenceClassSpec, ...] = (
    EvidenceClassSpec(
        "timeout-calibration-baselines",
        _OWNER_EVIDENCE,
        "three clean per-gate timeout-calibration baselines",
    ),
    EvidenceClassSpec(
        "verifier-fault-campaigns",
        _OWNER_EVIDENCE,
        "three consecutive verifier fault campaigns with no surviving process, port, lease, or pointer",
    ),
    EvidenceClassSpec(
        "coverage-and-mutation-thresholds",
        _OWNER_EVIDENCE,
        "global and critical-domain coverage plus backend/frontend mutation thresholds",
    ),
    EvidenceClassSpec(
        "architecture-contract",
        _OWNER_EVIDENCE,
        "mechanically verified module-size ceilings, dependency direction, generated IPC, and zero command/handler SQL or component-level runtime imports",
    ),
    EvidenceClassSpec(
        "known-defect-ledger",
        _OWNER_EVIDENCE,
        "machine-audited release defect inventory with zero unresolved P0, P1, or supported-flow P2",
    ),
    EvidenceClassSpec(
        "schema-clone-and-restore-campaign",
        _OWNER_EVIDENCE,
        "fresh install, supported-boundary migration, live-sized clone, snapshots, and isolated restore",
    ),
    EvidenceClassSpec(
        "concurrency-performance-and-memory-campaign",
        _OWNER_EVIDENCE,
        "50,000-segment concurrency hammer, 100,000-segment budgets, and 1,000-decision heap soak",
    ),
    EvidenceClassSpec(
        "owner-workflow-and-recovery-campaign",
        _OWNER_EVIDENCE,
        "real champion workflow plus wrong-model, crash, disk-full, corrupt-DB, lost-response, and kill drills",
    ),
    EvidenceClassSpec(
        "owner-deployment-reboot-runs",
        _OWNER_EVIDENCE,
        "three no-retry exact-commit verifier runs before deployment, after deployment, and after cold reboot",
    ),
    EvidenceClassSpec(
        "owner-field-sessions",
        _OWNER_EVIDENCE,
        "thirty owner daily-use sessions with no P0, P1, or supported core-loop P2 incident",
    ),
    EvidenceClassSpec(
        "owner-product-attestation",
        _WINDOWS_EVIDENCE,
        "validated owner-product proof and attestation for the same source and executable identity",
    ),
    EvidenceClassSpec(
        "signed-windows-release-artifacts",
        _WINDOWS_EVIDENCE,
        "Authenticode/timestamped MSI, NSIS and updater plus checksums, SBOM, provenance, and signed tag",
    ),
    EvidenceClassSpec(
        "supported-windows-vm-campaign",
        _WINDOWS_EVIDENCE,
        "clean install on the two newest supported Windows 11 servicing releases",
    ),
    EvidenceClassSpec(
        "windows-update-rollback-uninstall-campaign",
        _WINDOWS_EVIDENCE,
        "update, interrupted update, signature rejection, rollback, and data-preserving uninstall",
    ),
    EvidenceClassSpec(
        "windows-manual-accessibility",
        _WINDOWS_EVIDENCE,
        "NVDA, keyboard, 200/400% zoom, high contrast, text scaling, reduced motion, and focus evidence",
    ),
    EvidenceClassSpec(
        "windows-comparator-study",
        _WINDOWS_EVIDENCE,
        "paired eight-participant benchmark meeting every Gate B threshold",
    ),
    EvidenceClassSpec(
        "windows-five-user-pilot",
        _WINDOWS_EVIDENCE,
        "seven-day five-user Windows pilot with no supported-flow release blocker",
    ),
    EvidenceClassSpec(
        "current-model-attestation",
        _MODEL_EVIDENCE,
        "current reproducible hash-bound model attestation",
    ),
    EvidenceClassSpec(
        "gold-marathon-and-iaa",
        _MODEL_EVIDENCE,
        "Gold Marathon plus independent-annotator IAA evidence",
    ),
    EvidenceClassSpec(
        "cordi-dialect-domain-evidence",
        _MODEL_EVIDENCE,
        "CORDI dialect/domain evidence",
    ),
)

# Artifact roles are part of the certification contract, independent of prose evidence.  A future
# evidence validator cannot turn a profile green while silently omitting the exact binary/bundle
# classes whose hashes must be carried by ProductAttestationV1.
PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES: dict[str, tuple[str, ...]] = {
    PROFILE_OWNER: ("application-executable",),
    PROFILE_WINDOWS: (
        "application-executable",
        "windows-msi",
        "windows-nsis",
        "windows-updater",
        "release-checksums",
        "cyclonedx-sbom",
        "github-sigstore-provenance",
    ),
    PROFILE_MODEL: (),
}
PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[PROFILE_FULL] = tuple(
    dict.fromkeys(
        role
        for profile_name in (PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_MODEL)
        for role in PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[profile_name]
    )
)


def _required_evidence_specs(profile: str) -> tuple[EvidenceClassSpec, ...]:
    if profile not in PROFILES:
        raise ValueError(f"unknown certification profile {profile!r}")
    return tuple(spec for spec in EVIDENCE_CLASSES if profile in spec.profiles)


def evidence_contract_document() -> dict[str, object]:
    return {
        "schema": 1,
        "policy": (
            "Evidence is pending unless a verifier-owned class-specific validator emits it; "
            "self-authored pass flags are never accepted."
        ),
        "freshnessPolicy": {
            "state": "PARTIAL_CLASS_SPECIFIC_FAIL_CLOSED",
            "rule": (
                "A VERIFIED class must carry validator-produced measuredAt plus either an expiresAt "
                "that is checked again when the proof is published and consumed, or a hash-bound "
                "immutableAuthority explaining why expiry does not apply. Implemented validators are "
                "recomputed from their exact gate artifacts when the proof is consumed. Until class-specific "
                "logic exists, the class remains PENDING_EXTERNAL; no global magic time window is inferred."
            ),
        },
        "classes": [
            {
                "id": spec.id,
                "profiles": sorted(spec.profiles),
                "description": spec.description,
                "validatorGate": EVIDENCE_VALIDATOR_GATES.get(spec.id, (None, (), ""))[0],
            }
            for spec in EVIDENCE_CLASSES
        ],
        "requiredReleaseArtifactRoles": {
            profile: list(PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[profile])
            for profile in sorted(PROFILES)
        },
    }


def evidence_contract_hash() -> str:
    canonical = json.dumps(
        evidence_contract_document(), ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def _pending_evidence_results(profile: str) -> list[dict[str, str]]:
    return [
        {
            "classId": spec.id,
            "status": PENDING_EXTERNAL,
            "detail": spec.description,
        }
        for spec in _required_evidence_specs(profile)
    ]


@dataclass(frozen=True)
class GateStep:
    argv: tuple[str, ...]


@dataclass(frozen=True)
class GateSpec:
    """The complete, hashable execution contract for one verifier gate.

    ``__iter__`` and ``__getitem__`` retain the old seven-field read-only policy-test API while the
    runtime uses typed steps, explicit timeouts, artifacts, profiles and retry semantics.
    """

    id: str
    tier: int
    profiles: frozenset[str]
    kind: str
    payload: object
    steps: tuple[GateStep, ...]
    cwd: Path
    environment_probe: Callable[[], object] | None
    environment_allowlist: tuple[str, ...]
    timeout_seconds: int
    artifact_requirements: tuple[str, ...]
    retry_policy: str
    charter_ref: str

    def legacy_tuple(self) -> tuple[object, ...]:
        legacy_cwd: Path | None = self.cwd if self.kind == "cmd" else None
        return (
            self.id,
            self.tier,
            self.kind,
            self.payload,
            legacy_cwd,
            self.environment_probe,
            self.charter_ref,
        )

    def __iter__(self) -> Iterator[object]:
        return iter(self.legacy_tuple())

    def __getitem__(self, index: int) -> object:
        return self.legacy_tuple()[index]


def _command_argv(command: str) -> tuple[str, ...]:
    """Parse one command line with Windows' own quoting rules; never invoke a shell implicitly."""

    if os.name != "nt":
        parsed = shlex.split(command)
    else:
        shell32 = ctypes.WinDLL("shell32", use_last_error=True)
        argc = ctypes.c_int()
        shell32.CommandLineToArgvW.restype = ctypes.POINTER(ctypes.c_wchar_p)
        pointer = shell32.CommandLineToArgvW(command, ctypes.byref(argc))
        if not pointer:
            raise ValueError(f"cannot parse gate command: {command!r}")
        try:
            parsed = [pointer[index] for index in range(argc.value)]
        finally:
            ctypes.WinDLL("kernel32", use_last_error=True).LocalFree(pointer)
    if not parsed:
        raise ValueError("gate command has no executable")
    resolved = shutil.which(parsed[0]) or parsed[0]
    parsed[0] = resolved
    if os.name == "nt" and Path(resolved).suffix.casefold() in {".cmd", ".bat"}:
        # Batch files are an explicit interpreter substep. Popen still receives an argument array
        # and ``shell=False``; metacharacters can never join otherwise independent gate steps.
        command_processor = os.environ.get("COMSPEC", r"C:\Windows\System32\cmd.exe")
        return (command_processor, "/d", "/s", "/c", subprocess.list2cmdline(parsed))
    return tuple(parsed)


def _command_steps(command: str) -> tuple[GateStep, ...]:
    parts = command.split(" && ")
    if any(not part.strip() for part in parts):
        raise ValueError(f"gate command has an empty compound substep: {command!r}")
    return tuple(GateStep(_command_argv(part.strip())) for part in parts)


def _profiles_for_gate(name: str, tier: int) -> frozenset[str]:
    # Every verdict, including model-only evidence, names one exact release commit.  A checkout
    # digest records dirty bytes for diagnostics but does not turn them into committed source.
    if name == "clean-source-tree":
        return PROFILES
    profiles = {PROFILE_FULL}
    model_gates = {
        "license-compat",
        "dataset-duplicates",
        "snapshot-immutability",
        "challenger-loop",
        "review-serving-provenance",
        "fuzz-smoke",
        "fairness-gender-age",
        "refinery-lift",
    }
    if name in model_gates:
        profiles.add(PROFILE_MODEL)
    owner_product_gate = name not in model_gates or name in {
        "dataset-duplicates",
        "review-serving-provenance",
        "fuzz-smoke",
    }
    if owner_product_gate:
        # Windows certification is layered on the owner/core product, not an alternate shallow
        # profile.  The prior tier<=1 shortcut let a signed shell bypass database/review truth,
        # champion operation, and the durability/export kill drills entirely.
        profiles.update({PROFILE_OWNER, PROFILE_WINDOWS})
    return frozenset(profiles)


def _timeout_for_gate(name: str, kind: str) -> int:
    """Explicit provisional budgets; certification remains blocked until three baselines calibrate them."""

    if kind == "fn":
        return 120
    return {
        "python-policies": 1_500,
        "test-rust": 1_800,
        "clippy": 900,
        "test-e2e+a11y": 900,
        "real-app-e2e": 900,
        "fuzz-smoke": 1_200,
        "pipeline-ipc-e2e": 900,
        "durability-drill": 1_200,
        "export-kill-drill": 900,
    }.get(name, 240)


GATE_BASE_ENVIRONMENT = (
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "CI",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "NPM_CONFIG_CACHE",
    "WSLENV",
    "WSL_DISTRO_NAME",
    # This is verifier-owned (forced to "1" below), not caller authority. Every harness can use it
    # to refuse foreign ports/servers while still receiving no ambient Cortex data override.
    "CORTEX_GATE",
)

# Caller-controlled Cortex variables are never ambient gate inputs. Non-live disposable harnesses
# receive only the exact variables declared in GATE_ENVIRONMENT_BY_ID. Live-state gates receive none
# of the authority overrides below during a certifying run; explicit diagnostic mode may pass them
# through but permanently makes the whole run non-certifying.
LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT = (
    "CORTEX_7B_PORT",
    "CORTEX_APP_DATA_DIR",
    "CORTEX_APP_EXE",
    "CORTEX_ASR_ENGINE",
    "CORTEX_BUILD_SHA",
    "CORTEX_CHAMPION_POINTER",
    "CORTEX_COUCH_PORT",
    "CORTEX_DB",
    "CORTEX_DB_DIR",
    "CORTEX_DEBUG_PORT",
    "CORTEX_DISK_FLOOR_GB",
)

REDACTED_PATH_ENVIRONMENT = frozenset(
    {
        "APPDATA",
        "LOCALAPPDATA",
        "CORTEX_APP_DATA_DIR",
        "CORTEX_APP_EXE",
        "CORTEX_CHAMPION_POINTER",
        "CORTEX_DB",
        "CORTEX_DB_DIR",
    }
)

LIVE_AUTHORITY_GATE_IDS = frozenset(
    {
        "spot-check-pool",
        "dataset-duplicates",
        "snapshot-immutability",
        "challenger-loop",
        "reviewer-queues-live",
        "review-serving-provenance",
        "database-integrity-live",
        "review-schema-contract-live",
        "review-compensation-readiness",
        "review-mode-certification",
        "reviewer-links-live",
        "exe-freshness",
        "playback-enforcement-readiness",
        "supervision-live",
        "champion-7b-preflight",
    }
)

AUTHORITY_MODE_LIVE = "windows-known-folders-live"
AUTHORITY_MODE_DIAGNOSTIC = "diagnostic-caller-overrides"
AUTHORITY_MODES = frozenset({AUTHORITY_MODE_LIVE, AUTHORITY_MODE_DIAGNOSTIC})
RUN_AUTHORITY_NAME = "live-authority.json"
ROAMING_APP_DATA_FOLDER_ID = "3eb685db-65f9-4cf6-a03a-e3ef65729f3d"
LOCAL_APP_DATA_FOLDER_ID = "f1b32785-6fba-4fcf-9d55-7b8e7f157091"

GATE_ENVIRONMENT_BY_ID: dict[str, tuple[str, ...]] = {
    # Remote branch-policy inspection is the only gate that needs repository read authority. Its
    # token is not exposed to npm, cargo, app binaries, or evidence scripts.
    "branch-protection": ("GH_TOKEN", "GITHUB_TOKEN"),
    # These are disposable/non-live harness inputs. Their presence is redacted and hash-bound in
    # the gate result, while the live database gates below remain isolated from them.
    "test-rust": ("CORTEX_REAL_AUDIO_DIR",),
    "test-e2e+a11y": ("CORTEX_GATE", "CORTEX_SMOKE_TEST"),
    "real-app-e2e": (
        "CORTEX_GATE",
        "CORTEX_APP_EXE",
        "CORTEX_AUDIO",
        "CORTEX_ASR_ENGINE",
        "CORTEX_DEBUG_PORT",
    ),
    "egress-runtime": (
        "CORTEX_APP_EXE",
        "CORTEX_DEBUG_PORT",
        "CORTEX_EGRESS_AUDIO",
        "CORTEX_EGRESS_SAMPLE_MS",
        "CORTEX_EGRESS_TRANSCRIBE",
        "CORTEX_EGRESS_WORKLOAD_MS",
        "CORTEX_7B_PORT",
    ),
    "pipeline-ipc-e2e": (
        "CORTEX_GATE",
        "CORTEX_APP_EXE",
        "CORTEX_AUDIO",
        "CORTEX_DEBUG_PORT",
        "CORTEX_7B_PORT",
    ),
    "heartbeat-runtime": (
        "CORTEX_APP_EXE",
        "CORTEX_DEBUG_PORT",
        "CORTEX_HEARTBEAT_AUDIO",
        "CORTEX_HEARTBEAT_MAX_MS",
    ),
    "jobs-runtime": ("CORTEX_APP_EXE", "CORTEX_DEBUG_PORT"),
    # This is a verifier-owned fail-closed switch, never a caller preference. Without it the
    # ignored Rust preflight reports a missing live champion as a successful skipped test.
    "champion-7b-preflight": ("CORTEX_REQUIRE_7B",),
}

GATE_FORCED_ENVIRONMENT_BY_ID: dict[str, dict[str, str]] = {
    "champion-7b-preflight": {"CORTEX_REQUIRE_7B": "1"},
}

GATE_ARTIFACT_REQUIREMENTS_BY_ID: dict[str, tuple[str, ...]] = {
    "architecture-contract-evidence": ("architecture-contract.json",),
    "known-defect-ledger-evidence": ("known-defect-ledger.json",),
}

RUST_COVERAGE_ENVIRONMENT_ALLOWLIST = GATE_BASE_ENVIRONMENT


class _WindowsGuid(ctypes.Structure):
    _fields_ = [
        ("data1", ctypes.c_uint32),
        ("data2", ctypes.c_uint16),
        ("data3", ctypes.c_uint16),
        ("data4", ctypes.c_ubyte * 8),
    ]


def _windows_guid(value: str) -> _WindowsGuid:
    raw = uuid.UUID(value).bytes_le
    return _WindowsGuid.from_buffer_copy(raw)


def _windows_known_folder(folder_id: str) -> Path:
    """Resolve a Windows Known Folder without trusting APPDATA/USERPROFILE environment text."""

    if os.name != "nt":
        raise EvidenceError("live product authority requires Windows Known Folder resolution")
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
    ole32 = ctypes.WinDLL("ole32", use_last_error=True)
    sh_get_known_folder = shell32.SHGetKnownFolderPath
    sh_get_known_folder.argtypes = [
        ctypes.POINTER(_WindowsGuid),
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_wchar_p),
    ]
    sh_get_known_folder.restype = ctypes.c_long
    ole32.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    ole32.CoTaskMemFree.restype = None
    guid = _windows_guid(folder_id)
    pointer = ctypes.c_wchar_p()
    result = sh_get_known_folder(ctypes.byref(guid), 0, None, ctypes.byref(pointer))
    if result != 0 or not pointer.value:
        raise EvidenceError(
            f"Windows Known Folder {folder_id} cannot be resolved (HRESULT 0x{result & 0xFFFFFFFF:08x})"
        )
    try:
        return Path(pointer.value).resolve(strict=True)
    finally:
        ole32.CoTaskMemFree(ctypes.cast(pointer, ctypes.c_void_p))


def _canonical_live_data_roots() -> tuple[Path, Path]:
    return (
        _windows_known_folder(ROAMING_APP_DATA_FOLDER_ID),
        _windows_known_folder(LOCAL_APP_DATA_FOLDER_ID),
    )


def _redacted_path_digest(path: Path) -> str:
    normalized = os.path.normcase(str(path.resolve(strict=False))).encode("utf-8")
    return hashlib.sha256(normalized).hexdigest()


def _document_digest(document: dict[str, object]) -> str:
    canonical = json.dumps(document, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def _run_authority_document(
    *,
    diagnostic_overrides: bool,
    caller_environment: dict[str, str] | None = None,
) -> dict[str, object]:
    caller = dict(os.environ if caller_environment is None else caller_environment)
    mode = AUTHORITY_MODE_DIAGNOSTIC if diagnostic_overrides else AUTHORITY_MODE_LIVE
    if diagnostic_overrides:
        appdata_value = caller.get("APPDATA")
        localappdata_value = caller.get("LOCALAPPDATA")
        if not appdata_value or not localappdata_value:
            raise EvidenceError("diagnostic live-authority override mode requires APPDATA and LOCALAPPDATA")
        appdata = Path(appdata_value).resolve(strict=False)
        localappdata = Path(localappdata_value).resolve(strict=False)
        root_source = "caller-environment"
        appdata_folder_id: str | None = None
        localappdata_folder_id: str | None = None
    else:
        appdata, localappdata = _canonical_live_data_roots()
        root_source = "windows-known-folder"
        appdata_folder_id = ROAMING_APP_DATA_FOLDER_ID
        localappdata_folder_id = LOCAL_APP_DATA_FOLDER_ID

    data_directory = appdata / "cortex-speech"
    database = data_directory / "cortex-speech.db"
    active_release_pointer = data_directory / ACTIVE_RELEASE_POINTER
    release_root = localappdata / "CortexSpeech" / "private-production-releases"
    caller_overrides = sorted(
        name for name in LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT if name in caller
    )
    caller_root_differences = sorted(
        name
        for name, observed, authoritative in (
            ("APPDATA", caller.get("APPDATA"), appdata),
            ("LOCALAPPDATA", caller.get("LOCALAPPDATA"), localappdata),
        )
        if observed is not None
        and _redacted_path_digest(Path(observed)) != _redacted_path_digest(authoritative)
    )
    document: dict[str, object] = {
        "schema": 1,
        "type": "LiveAuthorityV1",
        "mode": mode,
        "certificationEligible": not diagnostic_overrides,
        "roots": {
            "roamingAppData": {
                "source": root_source,
                "knownFolderId": appdata_folder_id,
                "absolutePathSha256": _redacted_path_digest(appdata),
            },
            "localAppData": {
                "source": root_source,
                "knownFolderId": localappdata_folder_id,
                "absolutePathSha256": _redacted_path_digest(localappdata),
            },
        },
        "targets": {
            "dataDirectory": {
                "base": "roamingAppData",
                "relativePath": "cortex-speech",
                "absolutePathSha256": _redacted_path_digest(data_directory),
            },
            "database": {
                "base": "roamingAppData",
                "relativePath": "cortex-speech/cortex-speech.db",
                "absolutePathSha256": _redacted_path_digest(database),
            },
            "activeReleasePointer": {
                "base": "roamingAppData",
                "relativePath": f"cortex-speech/{ACTIVE_RELEASE_POINTER}",
                "absolutePathSha256": _redacted_path_digest(active_release_pointer),
            },
            "privateReleaseRoot": {
                "base": "localAppData",
                "relativePath": "CortexSpeech/private-production-releases",
                "absolutePathSha256": _redacted_path_digest(release_root),
            },
        },
        "callerOverrides": {
            "policy": "diagnostic-active" if diagnostic_overrides else "ignored",
            "names": caller_overrides,
            "rootDifferences": caller_root_differences,
        },
    }
    return {**document, "authorityDigest": _document_digest(document)}


def _prepare_run_authority(diagnostic_overrides: bool) -> dict[str, object]:
    caller = dict(os.environ)
    document = _run_authority_document(
        diagnostic_overrides=diagnostic_overrides,
        caller_environment=caller,
    )
    if not diagnostic_overrides:
        appdata, localappdata = _canonical_live_data_roots()
        # The verifier process itself also resolves release artifacts. Canonicalize its view so
        # root-level attestation code cannot observe caller-substituted live state or release bytes.
        os.environ["APPDATA"] = str(appdata)
        os.environ["LOCALAPPDATA"] = str(localappdata)
        for name in LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT:
            os.environ.pop(name, None)
    return document


def _validate_run_authority(value: object) -> tuple[str, str]:
    if not isinstance(value, dict) or value.get("schema") != 1 or value.get("type") != "LiveAuthorityV1":
        raise EvidenceError("proof live authority has the wrong schema/type")
    if set(value) != {
        "schema",
        "type",
        "mode",
        "certificationEligible",
        "roots",
        "targets",
        "callerOverrides",
        "authorityDigest",
    }:
        raise EvidenceError("proof live authority has a non-canonical envelope")
    mode = value.get("mode")
    digest = value.get("authorityDigest")
    if mode not in AUTHORITY_MODES or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise EvidenceError("proof live authority has an invalid mode/digest")
    unsigned = {key: item for key, item in value.items() if key != "authorityDigest"}
    if _document_digest(unsigned) != digest:
        raise EvidenceError("proof live-authority digest is invalid")
    if value.get("certificationEligible") is not (mode == AUTHORITY_MODE_LIVE):
        raise EvidenceError("proof live-authority eligibility contradicts its mode")
    roots = value.get("roots")
    targets = value.get("targets")
    overrides = value.get("callerOverrides")
    if not isinstance(roots, dict) or not isinstance(targets, dict) or not isinstance(overrides, dict):
        raise EvidenceError("proof live authority omits roots, targets, or override policy")
    if set(roots) != {"roamingAppData", "localAppData"}:
        raise EvidenceError("proof live authority has an incomplete root inventory")
    for root_name in ("roamingAppData", "localAppData"):
        root = roots.get(root_name)
        if (
            not isinstance(root, dict)
            or set(root) != {"source", "knownFolderId", "absolutePathSha256"}
            or not re.fullmatch(r"[0-9a-f]{64}", str(root.get("absolutePathSha256", "")))
        ):
            raise EvidenceError(f"proof live authority root {root_name} is malformed")
    expected_target_paths = {
        "dataDirectory": ("roamingAppData", "cortex-speech"),
        "database": ("roamingAppData", "cortex-speech/cortex-speech.db"),
        "activeReleasePointer": (
            "roamingAppData",
            f"cortex-speech/{ACTIVE_RELEASE_POINTER}",
        ),
        "privateReleaseRoot": (
            "localAppData",
            "CortexSpeech/private-production-releases",
        ),
    }
    if set(targets) != set(expected_target_paths):
        raise EvidenceError("proof live authority has an incomplete target inventory")
    for name, (base, relative) in expected_target_paths.items():
        target = targets.get(name)
        if (
            not isinstance(target, dict)
            or set(target) != {"base", "relativePath", "absolutePathSha256"}
            or target.get("base") != base
            or target.get("relativePath") != relative
            or not re.fullmatch(r"[0-9a-f]{64}", str(target.get("absolutePathSha256", "")))
        ):
            raise EvidenceError(f"proof live authority target {name} is malformed or substituted")
    override_names = overrides.get("names")
    root_differences = overrides.get("rootDifferences")
    if (
        set(overrides) != {"policy", "names", "rootDifferences"}
        or not isinstance(override_names, list)
        or override_names != sorted(set(override_names))
        or not set(override_names) <= set(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
        or not isinstance(root_differences, list)
        or root_differences != sorted(set(root_differences))
        or not set(root_differences) <= {"APPDATA", "LOCALAPPDATA"}
    ):
        raise EvidenceError("proof live authority has a malformed caller-override inventory")
    if mode == AUTHORITY_MODE_LIVE:
        expected_roots = {
            "roamingAppData": ROAMING_APP_DATA_FOLDER_ID,
            "localAppData": LOCAL_APP_DATA_FOLDER_ID,
        }
        for name, folder_id in expected_roots.items():
            root = roots.get(name)
            if (
                not isinstance(root, dict)
                or root.get("source") != "windows-known-folder"
                or root.get("knownFolderId") != folder_id
            ):
                raise EvidenceError("certifying live authority is not Windows Known Folder bound")
        if overrides.get("policy") != "ignored":
            raise EvidenceError("certifying live authority activated caller overrides")
    else:
        for name in ("roamingAppData", "localAppData"):
            root = roots[name]
            if root.get("source") != "caller-environment" or root.get("knownFolderId") is not None:
                raise EvidenceError("diagnostic live authority does not disclose caller roots")
        if overrides.get("policy") != "diagnostic-active":
            raise EvidenceError("diagnostic live authority does not disclose active override mode")
    return str(mode), digest


def _rust_coverage_environment() -> dict[str, str]:
    environment = {
        key: value
        for key in RUST_COVERAGE_ENVIRONMENT_ALLOWLIST
        if (value := os.environ.get(key)) is not None
    }
    environment["PYTHONUTF8"] = "1"
    environment["PYTHONIOENCODING"] = "utf-8"
    environment["CORTEX_GATE"] = "1"
    environment["CARGO_NET_OFFLINE"] = "true"
    environment["CARGO_INCREMENTAL"] = "0"
    return environment


def _typed_gate(row: Sequence[object]) -> GateSpec:
    name, tier, kind, payload, cwd, probe, charter = row
    if not isinstance(name, str) or not isinstance(tier, int) or not isinstance(kind, str):
        raise TypeError(f"invalid gate registry row: {row!r}")
    if kind == "cmd":
        if not isinstance(payload, str):
            raise TypeError(f"command gate {name} has non-string payload")
        steps = _command_steps(payload)
    elif kind in {"fn", "not-built"}:
        steps = ()
    else:
        raise ValueError(f"gate {name} has unknown kind {kind}")
    resolved_cwd = Path(cwd or REPO_ROOT).resolve()
    return GateSpec(
        id=name,
        tier=tier,
        profiles=_profiles_for_gate(name, tier),
        kind=kind,
        payload=payload,
        steps=steps,
        cwd=resolved_cwd,
        environment_probe=probe if callable(probe) else None,
        environment_allowlist=tuple(
            dict.fromkeys((*GATE_BASE_ENVIRONMENT, *GATE_ENVIRONMENT_BY_ID.get(name, ())))
        ),
        timeout_seconds=_timeout_for_gate(name, kind),
        artifact_requirements=(
            "attempt-log",
            "worker-result",
            *GATE_ARTIFACT_REQUIREMENTS_BY_ID.get(name, ()),
        ),
        retry_policy="diagnostic-once",
        charter_ref=str(charter),
    )


KNOWN_DEFECT_LEDGER = REPO_ROOT / "docs" / "KNOWN_DEFECTS.v1.json"
_ARCHITECTURE_ARTIFACT = "architecture-contract.json"
_KNOWN_DEFECT_ARTIFACT = "known-defect-ledger.json"


def _load_json_without_duplicate_keys(path: Path) -> object:
    def pairs_hook(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise EvidenceError(f"{path.name} contains duplicate JSON key {key!r}")
            result[key] = value
        return result

    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=pairs_hook)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot read strict JSON authority {path}: {error}") from error


def _tracked_authority_binding(path: Path, full_sha: str) -> dict[str, object]:
    """Bind an authority to Git's canonical blob bytes, independent of checkout EOL translation.

    The evidence class requires ``clean-source-tree`` before it can verify, so a dirty checkout can
    never certify. Reading the blob here avoids treating Git's normal LF/CRLF materialization on
    Windows as source drift while still hashing the exact bytes identified by ``full_sha``.
    """

    resolved = path.resolve(strict=True)
    try:
        relative = resolved.relative_to(REPO_ROOT.resolve()).as_posix()
    except ValueError as error:
        raise EvidenceError(f"authority path escapes the repository: {path}") from error
    committed = _git_file_bytes(full_sha, relative)
    return {
        "path": relative,
        "gitBlobSha1": _git_blob_id(full_sha, relative),
        "sha256": hashlib.sha256(committed).hexdigest(),
        "bytes": len(committed),
    }


def _safe_tracked_path(value: object, label: str) -> Path:
    if not isinstance(value, str) or not value or "\\" in value:
        raise EvidenceError(f"{label} must be a non-empty repository-relative POSIX path")
    candidate = Path(value)
    if candidate.is_absolute() or any(part in {"", ".", ".."} for part in candidate.parts):
        raise EvidenceError(f"{label} is not a safe repository-relative path")
    resolved = (REPO_ROOT / candidate).resolve(strict=True)
    try:
        resolved.relative_to(REPO_ROOT.resolve())
    except ValueError as error:
        raise EvidenceError(f"{label} escapes the repository") from error
    return resolved


def _fn_known_defect_ledger() -> bool:
    """Audit the exact tracked defect inventory and emit one immutable-source evidence artifact."""

    artifact_path = LOG_DIR / _KNOWN_DEFECT_ARTIFACT
    full_sha = _full_git_sha()
    report: dict[str, object] = {
        "schema": 1,
        "classId": "known-defect-ledger",
        "fullGitSha": full_sha,
        "measuredAt": utc_now(),
        "immutableAuthority": "exact-git-commit",
        "passed": False,
        "failures": [],
    }
    try:
        ledger = _load_json_without_duplicate_keys(KNOWN_DEFECT_LEDGER)
        if not isinstance(ledger, dict) or set(ledger) != {
            "schema",
            "policy",
            "auditAuthorities",
            "defects",
        }:
            raise EvidenceError("known-defect ledger root must have the exact schema-1 field set")
        if (
            not isinstance(ledger.get("schema"), int)
            or isinstance(ledger.get("schema"), bool)
            or ledger.get("schema") != 1
        ):
            raise EvidenceError("known-defect ledger schema is not 1")
        policy = ledger.get("policy")
        if not isinstance(policy, dict) or set(policy) != {
            "blockingSeverities",
            "blockingStatuses",
            "rule",
        }:
            raise EvidenceError("known-defect policy has a non-exact shape")
        if policy.get("blockingSeverities") != ["P0", "P1", "P2"]:
            raise EvidenceError("known-defect policy weakened the P0/P1/P2 release floor")
        if policy.get("blockingStatuses") != ["OPEN", "REOPENED"]:
            raise EvidenceError("known-defect policy weakened open/reopened blocking status")
        if not isinstance(policy.get("rule"), str) or len(str(policy["rule"])) < 80:
            raise EvidenceError("known-defect policy does not state its release rule")

        authorities = ledger.get("auditAuthorities")
        if not isinstance(authorities, list) or not authorities:
            raise EvidenceError("known-defect ledger has no audit authority")
        authority_ids: set[str] = set()
        authority_bindings: list[dict[str, object]] = []
        for authority in authorities:
            if not isinstance(authority, dict) or set(authority) != {
                "id",
                "report",
                "remediation",
                "integrationMatrix",
            }:
                raise EvidenceError("known-defect audit authority has a non-exact shape")
            authority_id = authority.get("id")
            if (
                not isinstance(authority_id, str)
                or not re.fullmatch(r"[a-z0-9][a-z0-9-]{7,79}", authority_id)
                or authority_id in authority_ids
            ):
                raise EvidenceError("known-defect audit authority id is invalid or duplicated")
            authority_ids.add(authority_id)
            for field in ("report", "remediation", "integrationMatrix"):
                authority_bindings.append(
                    _tracked_authority_binding(
                        _safe_tracked_path(authority.get(field), f"auditAuthorities.{field}"),
                        full_sha,
                    )
                )

        defects = ledger.get("defects")
        if not isinstance(defects, list):
            raise EvidenceError("known-defect ledger defects must be an array")
        defect_ids: set[str] = set()
        normalized: list[dict[str, object]] = []
        blockers: list[str] = []
        allowed_profiles = {PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_MODEL, PROFILE_FULL}
        for defect in defects:
            if not isinstance(defect, dict) or set(defect) != {
                "id",
                "severity",
                "status",
                "supportedProfiles",
                "summary",
                "trackingAuthority",
            }:
                raise EvidenceError("known-defect row has a non-exact shape")
            defect_id = defect.get("id")
            if (
                not isinstance(defect_id, str)
                or not re.fullmatch(r"[A-Z0-9][A-Z0-9-]{5,79}", defect_id)
                or defect_id in defect_ids
            ):
                raise EvidenceError("known-defect id is invalid or duplicated")
            defect_ids.add(defect_id)
            severity = defect.get("severity")
            status = defect.get("status")
            profiles = defect.get("supportedProfiles")
            summary = defect.get("summary")
            if severity not in {"P0", "P1", "P2", "P3"}:
                raise EvidenceError(f"known defect {defect_id} has unknown severity")
            if status not in {"OPEN", "REOPENED", "FIXED", "CLOSED"}:
                raise EvidenceError(f"known defect {defect_id} has unknown status")
            if (
                not isinstance(profiles, list)
                or len(profiles) != len(set(profiles))
                or any(profile not in allowed_profiles for profile in profiles)
            ):
                raise EvidenceError(f"known defect {defect_id} has invalid supported profiles")
            if not isinstance(summary, str) or not summary.strip() or len(summary) > 500:
                raise EvidenceError(f"known defect {defect_id} has invalid summary")
            tracking = _tracked_authority_binding(
                _safe_tracked_path(
                    defect.get("trackingAuthority"),
                    f"defects.{defect_id}.trackingAuthority",
                ),
                full_sha,
            )
            if severity in policy["blockingSeverities"] and status in policy["blockingStatuses"] and profiles:
                blockers.append(defect_id)
            normalized.append(
                {
                    "id": defect_id,
                    "severity": severity,
                    "status": status,
                    "supportedProfiles": profiles,
                    "trackingAuthority": tracking,
                }
            )

        report.update(
            {
                "ledger": _tracked_authority_binding(KNOWN_DEFECT_LEDGER, full_sha),
                "auditAuthorities": authority_bindings,
                "defects": normalized,
                "blockingDefectIds": blockers,
                "passed": not blockers,
            }
        )
    except (EvidenceError, OSError, ValueError) as error:
        report["failures"] = [str(error)]
    atomic_write_json(artifact_path, report)
    return report.get("passed") is True


def _load_ipc_policy_module():
    source = APP / "scripts" / "test_ipc_contract_policy.py"
    name = "cortex_ipc_architecture_contract"
    spec = importlib.util.spec_from_file_location(name, source)
    if spec is None or spec.loader is None:
        raise EvidenceError("cannot load the committed IPC contract scanner")
    module = importlib.util.module_from_spec(spec)
    scripts_path = str(source.parent)
    inserted = scripts_path not in sys.path
    if inserted:
        sys.path.insert(0, scripts_path)
    try:
        spec.loader.exec_module(module)
    except Exception as error:  # noqa: BLE001 - a broken scanner is evidence failure
        raise EvidenceError(f"cannot execute the committed IPC contract scanner: {error}") from error
    finally:
        if inserted:
            sys.path.remove(scripts_path)
    return module


def _fn_architecture_contract() -> bool:
    """Measure the locked final architecture contract; current debt must produce a red artifact."""

    artifact_path = LOG_DIR / _ARCHITECTURE_ARTIFACT
    full_sha = _full_git_sha()
    failures: list[str] = []
    report: dict[str, object] = {
        "schema": 1,
        "classId": "architecture-contract",
        "fullGitSha": full_sha,
        "measuredAt": utc_now(),
        "immutableAuthority": "exact-git-commit",
        "passed": False,
    }
    try:
        rust_module = _rust_quality_module()
        rust_verdict = rust_module.evaluate_architecture()
        rust_document = rust_module._architecture_json(rust_verdict)
        failures.extend(f"rust: {item}" for item in rust_verdict.failures)

        ipc_module = _load_ipc_policy_module()
        handwritten, generated, dynamic = ipc_module.frontend_invocations()
        noncanonical_errors = ipc_module.generated_noncanonical_error_commands()
        if not generated:
            failures.append("ipc: generated command inventory is empty")
        if handwritten:
            failures.append(
                f"ipc: {len(handwritten)} handwritten command contract(s) remain; final target is zero"
            )
        if dynamic:
            failures.append(
                f"ipc: {len(dynamic)} dynamic command-name bridge(s) remain; final target is zero"
            )
        if noncanonical_errors:
            failures.append(
                "ipc: generated command(s) expose a non-CommandErrorV1 rejection contract: "
                + ", ".join(sorted(noncanonical_errors))
            )

        workspace_limits = {
            "src/Workstation.svelte": 500,
            "src/lib/ReviewMode.svelte": 500,
            "src/lib/ReviewInbox.svelte": 500,
            "src/lib/SettingsPanel.svelte": 500,
            "src/lib/StatsDashboard.svelte": 500,
        }
        frontend_measurements: list[dict[str, object]] = []
        svelte_files = sorted((APP / "src").rglob("*.svelte"))
        if not svelte_files:
            raise EvidenceError("frontend architecture scan found no Svelte files")
        for path in svelte_files:
            relative = path.relative_to(APP).as_posix()
            lines = len(path.read_text(encoding="utf-8").splitlines())
            if relative == "src/App.svelte":
                limit = 350
                kind = "composition-shell"
            elif relative in workspace_limits:
                limit = workspace_limits[relative]
                kind = "workspace-controller"
            else:
                limit = 350
                kind = "presentational-component"
            passed = lines <= limit
            if not passed:
                failures.append(
                    f"frontend: {relative} has {lines} lines above the {limit}-line {kind} ceiling"
                )
            source = path.read_text(encoding="utf-8")
            direct_runtime = bool(re.search(r"@tauri-apps|\binvoke\s*\(", source))
            if direct_runtime:
                failures.append(f"frontend: {relative} imports or invokes the desktop runtime directly")
            frontend_measurements.append(
                {
                    "path": relative,
                    "kind": kind,
                    "lines": lines,
                    "maxLines": limit,
                    "passed": passed,
                    "directDesktopRuntime": direct_runtime,
                    "sha256": sha256_file(path),
                }
            )

        report.update(
            {
                "rust": rust_document,
                "ipc": {
                    "generatedCount": len(generated),
                    "handwrittenCount": len(handwritten),
                    "dynamicCount": len(dynamic),
                    "noncanonicalErrorCount": len(noncanonical_errors),
                    "handwrittenCommands": sorted(handwritten),
                    "dynamicSites": sorted(dynamic),
                    "noncanonicalErrorCommands": sorted(noncanonical_errors),
                },
                "frontend": {
                    "measurements": frontend_measurements,
                    "compositionShellMaxLines": 350,
                    "workspaceControllerMaxLines": 500,
                    "presentationalComponentMaxLines": 350,
                },
            }
        )
    except (EvidenceError, OSError, ValueError) as error:
        failures.append(str(error))
    report["failures"] = failures
    report["passed"] = not failures
    atomic_write_json(artifact_path, report)
    return not failures


# (name, tier, kind, payload, cwd, env_probe, charter_ref)
#   kind "fn"  -> payload is a callable returning bool
#   kind "cmd" -> payload is a shell command string
GATES = [
    # Tier 0 — static governance (seconds)
    ("manifest-alignment", 0, "fn", check_manifests, None, None, "Git+integrity: versions byte-equal CHANGELOG"),
    ("repo-integrity", 0, "fn", check_repo_integrity, None, None, "Git+integrity: LICENSE/NOTICE/repo URL"),
    ("clean-source-tree", 0, "fn", check_clean_source_tree, None, None, "Git+integrity: no tracked or untracked release inputs outside HEAD"),
    ("required-files", 0, "fn", check_required_files, None, None, "Engineering rigor: SECURITY.md/CODEOWNERS present"),
    ("ledger-schema", 0, "fn", check_provenance_ledger, None, None, "Data governance: ledger schema-valid"),
    ("license-compat", 0, "fn", check_license_compatibility, None, None, "Data governance: contamination gate"),
    # Tier 1 — CI-equivalent code gates (minutes)
    ("branch-protection", 1, "fn", check_branch_protection, None, _probe_branch_protection, "Git+integrity: main is protected on the remote, admins included (was OWNER_GATED item 49 - clicks done 2026-08-08, now machine-verified every sweep)"),
    ("python-policies", 1, "cmd", "npm run test:python-policies", APP, None, "honesty/privacy/CI/dataset policy tests"),
    ("rust-architecture-truth", 1, "cmd", f'"{sys.executable}" scripts/rust_quality_gate.py architecture', APP, None, "Fail-closed shipped-Rust architecture ceiling: every production module stays below 2,000 logical lines unless an exact SHA-bound immutable-history exception applies. Oversized production modules are RED, never warnings."),
    ("architecture-contract-evidence", 1, "fn", _fn_architecture_contract, None, None, "Hash-bound final architecture contract: Rust module ceilings, zero handwritten/dynamic IPC, bounded frontend workspaces/components, and no component-level desktop runtime imports."),
    ("known-defect-ledger-evidence", 1, "fn", _fn_known_defect_ledger, None, None, "Hash-bound strict known-defect inventory with zero OPEN/REOPENED P0, P1 or P2 defect in any supported release profile."),
    ("spot-check-pool", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_spot_check_pool.py"}"', APP, None, "The listening-QC must cover the WHOLE accessible paid-review campaign, not merely be able to fire once. The gate mirrors live focus, roster, dialect, on-disk audio, prior per-reviewer scores, and the Rust queue/check cadence; it derives each reviewer's worst-case key requirement because no enforced quota prevents one eligible reviewer from draining the queue. MEASURED 2026-08-21: the Hawleri campaign exposed 1,293 work clips but only 0-2 fresh keys per reviewer, so the old floor-of-3 gate would have gone green after three owner edits and then silently stopped measuring. Answer keys must be genuine owner-adjudicated/is_gold rows; never synthetic."),
    ("dataset-duplicates", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_dataset_duplicates.py"}"', APP, None, "The same-recording-under-different-names audit, on the LIVE library. FOUND BY THE OWNER'S EARS 2026-08-17, not by any gate: one recording lived under three filenames as different ENCODES, so the byte fingerprint saw three distinct files — ~68 duplicate sentences entered the corpus and 33 were reviewed (paid) twice, and duplicate content across nominally-different recordings can straddle a train/test split. Signal: source-timeline offset AND transcript agreeing across different files. Baseline 70, ratchets DOWN only."),
    ("snapshot-immutability", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_snapshot_immutability.py"}"', APP, None, "Gate C of docs/PLAN_TRUE_10.md. A training run cites a dataset snapshot id, and every CER measured from the resulting model hangs off that citation. This proves, on the LIVE library, that the id IS the content hash of the manifest it sealed (not a label someone chose), that no id is reused, that the sealed config names its own id, and that any pack still on disk still hashes to the snapshot it claims. Without it, 'trained on snapshot X' is decoration and every number downstream is unanchored. SKIP-ENV until the first pack is exported — it reports on data that exists and never invents a pass."),
    ("challenger-loop", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_challenger_loop.py"}"', APP, None, "Gate D of docs/PLAN_TRUE_10.md. The retrain flywheel's danger is not that a challenger LOSES — a REJECT is a good outcome and passes this gate — it is a run that LOOKS finished: a record saying 'trained' for training that never happened, a verdict with no snapshot behind it, or a PROMOTE whose own numbers do not support it. Checks the chain (train_challenger / build_eval_slices / promotion_gate) is present and audits every run record and verdict on disk for internal consistency. SKIP-ENV until a canary has actually run: wiring is not evidence, and a gate that says OK for an unrun loop is the flattering kind."),
    ("reviewer-queues-live", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_reviewer_queues_live.py"}"', APP, None, "Every reviewer holding a live link has clips they are ALLOWED to review. MEASURED 2026-08-17: two independent bugs made five of eight reviewers' queues empty while the owner was paying them, and each hid the other. The 1,031 recovered clips were relinked into D:\\Kurdish Corpora\\sorani\\ZarPodcast while dialect.rs still mapped only their pre-recovery path, so they were UNMAPPED and the dialect check fails closed; meanwhile the roster file carried a \"_comment\" string, which a strict HashMap<String, Vec<String>> parse rejects outright, and that failure path is \"unrestricted\" — so the protection was simultaneously off for everyone. Every row, every JSON file and every Rust function read correctly in isolation; only computing what each NAMED reviewer would actually be served exposes it. supervision-live cannot: the server answers 200 for an empty queue."),
    ("review-serving-provenance", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_serving_provenance.py"}"', APP, None, "Honesty at the SERVING path, on the LIVE db: annotated_transcript is human-only, and every untouched clip serves the champion's own transcript. MEASURED 2026-08-12: 348 rows held machine text in the human field, so the phone review page served a stale paraphrase while the fresh champion drafts sat invisible — reviewers corrected words the speaker never said. Write-path checks passed the whole time; only reading the row the server actually serves catches this class."),
    ("typecheck", 1, "cmd", "npm run typecheck", APP, None, "svelte-check + tsc"),
    ("lint-js", 1, "cmd", "npm run lint", APP, None, "eslint"),
    ("clippy", 1, "cmd", f'cargo clippy --manifest-path "{MANIFEST}" --all-targets --all-features -- -D warnings', REPO_ROOT, None, "Engineering rigor: strict Clippy across every target and feature"),
    ("fmt-check", 1, "cmd", f'cargo fmt --manifest-path "{MANIFEST}" --all -- --check', REPO_ROOT, None, "rustfmt"),
    ("runtime-asset-integrity", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "fetch_models.py"}" --check', APP, None, "SHA-256 of every required runtime-support asset plus every optional ASR artifact already present. Missing optional 300M/1B/MMS is healthy; a partial or mismatched optional installation is RED. The externally served WSL7B identity is proven separately at the serving path."),
    ("test-frontend", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "assert_ran.py"}" --min 400 --kind vitest -- npm test', APP, None, "Full Vitest suite with a current anti-vacuity floor. MEASURED 2026-08-26: 404 tests passed; the 400 floor permits a small intentional consolidation while failing closed if a suite, include pattern, or discovery root silently disappears. `assert_ran` also fails when it cannot parse the test-count line."),
    ("frontend-coverage", 1, "cmd", "npm run test:coverage", APP, None, "Fail-closed complete frontend coverage: V8 must report at least 85% statements/lines and 80% branches/functions across every shipped TypeScript and Svelte source file. Passing test count, lazy loading, and untested advanced workspaces do not waive the global contract; JSON summary evidence is retained beside the human-readable table."),
    ("test-rust", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "assert_ran.py"}" --min 1700 --kind cargo -- cargo test --manifest-path "{MANIFEST}" --all-targets --all-features --jobs 4', REPO_ROOT, None, "All-target/all-feature Rust suite: Sorani goldens, restore and review authority, WER parity, holdout hash, ONNX manifest and proof metadata. MEASURED 2026-08-26: the library target alone discovered 1,717 tests after the final restore additions; the 1,700 floor fails closed on material discovery loss while allowing a small intentional consolidation."),
    ("durable-decision-latency", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "assert_ran.py"}" --min 1 --kind cargo -- cargo test --manifest-path "{MANIFEST}" --lib db::tests::the_durability_cost_per_decision_is_measured_not_assumed -- --ignored --exact --nocapture --test-threads=1', REPO_ROOT, None, "Reviewer-visible durability latency is measured alone, with the exact 250 ms/decision threshold retained in Rust. The parallel library suite runs hundreds of unrelated FULL/fsync SQLite tests at once and measured ~310 ms here while the identical isolated path measured 1.5 ms; cross-test disk contention is not product latency. This mandatory, anti-vacuity gate keeps the benchmark isolated without letting it disappear or silently match zero tests."),
    ("audit", 1, "cmd", "npm audit --omit=dev && npm ls --all", APP, None, "npm supply chain. `npm ls --all` is the second half deliberately: MEASURED 2026-08-06, `npm audit` reported 0 vulnerabilities while the INSTALLED tree was structurally invalid (ELSPROBLEMS: a hoisted picomatch@2 could not satisfy the `^3 || ^4` peer fdir asks for). A clean audit says 'no KNOWN CVE in what resolved'; it says nothing about whether the tree resolved correctly at all. Both halves, or the gate only proves half of supply chain."),
    ("deny", 1, "cmd", f'cargo deny --manifest-path "{MANIFEST}" check', REPO_ROOT, _probe_deny, "cargo supply chain"),
    ("test-e2e+a11y", 1, "cmd", "npm run test:e2e", APP, None, "A11y: axe WCAG 2.2 AA en+ckb/RTL (coverage assertion: WS2 follow-up)"),
    # Tier 2 — real binary on this machine (the personal-use core)
    ("database-integrity-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_database_integrity.py"}" --require-production-v58-repair', APP, None, "Whole LIVE SQLite truth, read-only and unskippable: quick_check and full integrity_check must each return exactly ok, foreign_key_check must return zero rows across every table, migration history must be exact, and the immutable v58 archives must prove the authorized 2,104+2,104 production repair by identity digest and provenance. Feature-specific gates cannot certify a database that is structurally healthy but missing its repair evidence."),
    ("review-schema-contract-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_schema_contract.py"}"', APP, None, "Exact LIVE review schema truth: every table, index, trigger, and view created by canonical compensation/effect and pool migrations 57, 60-65 must byte-semantically match this exact checkout; every ALTER-added column and foreign key must exist; same-name dummy safety triggers and unexpected triggers on protected tables are RED. A schema_migrations row alone is not proof that future writes remain protected."),
    ("review-compensation-readiness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_compensation_readiness.py"}"', APP, None, "Mode-selected compensation truth on the LIVE database. Legacy pilot mode requires the exact review-iqd-v1-2026-08-21 policy (edit 100%, unchanged accept 10%, valid reject 10%, skip 0% at 18,000 IQD/full-equivalent hour), one durable ledger consequence per event, balanced revisions/settlements/operations, and canonical work identities. Flexible-pool mode (introduced at schema 63; current authority schema 65) follows the owner's operational deferral: the legacy policy/schema remain immutable, the pilot policy must be absent, and no pool decision may leak into the legacy payable event/ledger namespace. Missing or mixed authority is RED."),
    ("review-mode-certification", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_pilot_certification.py"}"', APP, None, "Mode-selected final review authority. An active flexible pool (introduced at schema 63; current authority schema 65) must bind to the exact hash-verified immutable release and pass a fresh detached full-integrity certification for its exact registry, membership, audio, rights, disk, local/offsite snapshots and internally consistent resolution totals; review readiness does not impersonate final dataset completion. Without a flexible pool, the strict legacy Rubar/Alle canary remains mandatory: 10+10 corpus and 2+2 hidden decisions, zero skips, exact playback and complete ledger/operation receipts. Conflicting modes or missing evidence are RED."),
    ("reviewer-links-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_reviewer_links_live.py"}" --funnel --port 8737 --require-private-production', APP, None, "Every durable reviewer credential must authenticate through the advertised Tailscale Funnel and bind to its intended identity, exact live database, fixed production port, durable state, and active review mode. Flexible-pool mode requires its immutable pool registry and forbids a simultaneous legacy pilot; a pre-pool database still requires the exact controlled-pilot contract. The probe is read-only: it mints no cookie, evicts no phone session, leases no work and consumes no hidden-check key. Queue eligibility is independently proven by reviewer-queues-live. Public TLS verification remains enabled; missing Funnel/session/mode/links is RED, never skipped."),
    ("exe-freshness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_exe_freshness.py"}"', REPO_ROOT, _probe_exe, "Truth-in-advertising: exe compiled from HEAD"),
    ("playback-enforcement-readiness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_playback_enforcement_readiness.py"}" --active-release', APP, None, "Mode-selected listening proof for the exact deployed binary. Flexible-pool mode (introduced at schema 63; current authority schema 65) hash-verifies the immutable active release and audits effective pool decisions using their immutable served revision, decoded-PCM BLAKE3 hash, exact source span, duration and playback-guard version; legacy mode retains compensation-ledger revision authority. At least 20 post-build phone decisions across two reviewer browsers must each carry >=85% canonical raw-counter playback evidence. No skip probe, --since override or empty-window pass is allowed."),
    ("supervision-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_supervision_live.py"}"', REPO_ROOT, None, "Fitness to SERVE, not just to compile: the watchdog is enabled, every live reviewer link answers on 8737, and the data drive has room to write. MEASURED 2026-08-15: all three were false at once — CortexWatchdog left `Disabled` by the rebuild procedure, the app exited so five sent links were dead, and C: at 0 bytes had already broken the 10-minute DB snapshot ('periodic DB snapshot failed'). Every source-level gate was still capable of GREEN, because none of them looks at the machine."),
    ("real-app-e2e", 2, "cmd", f'node "{APP / "e2e_real_app.cjs"}"', APP, _probe_real_e2e, "Daily-use proof on a disposable profile: real exe + real audio + the exact pinned WSL7B champion + real transcript. CORTEX_GATE forces WSL7B, so an inherited diagnostic-engine override cannot weaken this gate."),
    # Tier 3 — deep proof legs (env-gated; skipped honestly when absent)
    ("egress-runtime", 3, "cmd", f'node "{APP / "scripts" / "egress_probe.cjs"}"', APP, _probe_egress, "Privacy: zero outbound TCP from the complete app process tree, including WebView2, during startup + browse, with a positive-control sampler. Standard coverage makes no ASR-path claim and never auto-runs an installed smaller model. An explicit CORTEX_EGRESS_TRANSCRIBE=1 diagnostic adds WSL7B transcription coverage on a disposable profile."),
    ("champion-7b-preflight", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --jobs 4 -- --ignored wsl_7b_preflight', REPO_ROOT, _probe_champion_7b, "The champion's preflight against the REAL OmniASR-7B server. The champion drafts every clip (owner rule 2026-08-11), so the check that it is reachable before an import starts is the difference between a halt and a library half-drafted by a weaker engine."),
    # Deliberately count-agnostic: the gate enumerates targets with `cargo fuzz list` and fails loud on an
    # empty list, so hardcoding a number here only creates a second place to go stale. It said "5" until
    # the `features` target was removed with the dead FbankExtractor module it fuzzed (iteration 231).
    ("fuzz-smoke", 3, "fn", _fn_fuzz_smoke, None, _probe_fuzz, "Engineering rigor: every fuzz target, 0 crashes"),
    ("refinery-lift", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --test refinery_lift -- --ignored refinery_lift_injected_error_benchmark --nocapture', REPO_ROOT, None, "Model evidence only: >=30% CER reduction at <=15% escalation in the fixed-seed, algorithm-only T0 benchmark. Its disposable database is pinned to schema 59 because schema 60+ intentionally forbids machine-authored review truth; product integration remains a separate Gold Marathon gate."),
    ("fairness-gender-age", 3, "cmd", f'"{sys.executable}" "{APP / "scripts" / "fairness_gate.py"}"', REPO_ROOT, None, "WS4: gender/age CER disparity budget on committed corpus metadata (CORDI dialect leg owner-gated)"),
    ("pipeline-ipc-e2e", 3, "cmd", f'node "{APP / "e2e_pipeline_ipc.cjs"}"', APP, _probe_champion_ipc_harness, "Import -> VAD -> exact pinned WSL7B -> persisted transcript over real IPC on a disposable profile, independent of webview rendering."),
    ("heartbeat-runtime", 3, "cmd", f'node "{APP / "scripts" / "heartbeat_probe.cjs"}"', APP, _probe_ipc_harness, "Main-thread safety PROVEN AT RUNTIME: get_settings latency while slow commands run concurrently. The static test_command_main_thread_policy/test_ui_thread_blocking_audit pin the source shape; this measures the actual UI responsiveness they exist to protect."),
    ("bench-budget", 3, "cmd", f'"{sys.executable}" "{APP / "scripts" / "bench_gate.py"}"', APP, _probe_bench, "Criterion wall-clock regression budget against a COMMITTED baseline (docs/bench_baseline.json). The charter asks for this via github-action-benchmark on every PR; that CI clause is NOT satisfied here and stays open - this enforces the budget on the reference machine, where the charter's latency numbers are defined. Per-bench thresholds derived from measured run-to-run noise, and benches too noisy to gate are NAMED every run rather than given a pass-anything limit."),
    ("jobs-runtime", 3, "cmd", f'node "{APP / "scripts" / "jobs_probe.cjs"}"', APP, _probe_exe, "Durable Job Supervisor at runtime: a REAL export_dataset run is recorded in get_jobs and reaches 'succeeded' - the run_tracked bracketing proven end to end, not only in unit tests."),
    ("durability-drill", 3, "cmd", _drill_cmd("durability_writer", "durability_drill.py", "--cycles 25"), APP, None, "Crash durability PROVEN, not asserted: 25 hard kills of the real writer (production Database::open_with_retry + insert_segment) across write-phase and boot-phase, verifying integrity_check ok, zero LOST journaled edits, a contiguous id space and a row count that never decreases. The single reliability property daily review depends on - the app dying must never cost work that was saved. It existed and NOTHING ran it (found 2026-08-02 by asking which scripts no gate references); an unrun drill is a claim."),
    ("export-kill-drill", 3, "cmd", _drill_cmd("export_writer", "export_kill_drill.py", "--cycles 15"), APP, None, "Atomic-write design under real kills: 15 mid-export TerminateProcess cycles proving every JOURNALED export parses complete with the full row count, and that NO final .json is ever torn (atomic temp+fsync+rename in atomic_file.rs is the design under test). Scope honesty: process kill, not power loss. Same find as the durability drill - written, never run."),
]
GATES = [_typed_gate(row) for row in GATES]


def gate_registry_document() -> dict[str, object]:
    return {
        "schema": 1,
        "evidenceContract": evidence_contract_document(),
        "gates": [
            {
                "id": gate.id,
                "tier": gate.tier,
                "profiles": sorted(gate.profiles),
                "kind": gate.kind,
                "steps": [list(step.argv) for step in gate.steps],
                "cwd": str(gate.cwd),
                "timeoutSeconds": gate.timeout_seconds,
                "environmentAllowlist": list(gate.environment_allowlist),
                "forcedEnvironment": dict(
                    sorted(GATE_FORCED_ENVIRONMENT_BY_ID.get(gate.id, {}).items())
                ),
                "liveAuthorityGate": gate.id in LIVE_AUTHORITY_GATE_IDS,
                "diagnosticOverrideAllowlist": (
                    list(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
                    if gate.id in LIVE_AUTHORITY_GATE_IDS
                    else []
                ),
                "artifactRequirements": list(gate.artifact_requirements),
                "retryPolicy": gate.retry_policy,
                "charterRef": gate.charter_ref,
            }
            for gate in GATES
        ],
    }


def gate_registry_hash() -> str:
    canonical = json.dumps(
        gate_registry_document(), ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def _gate_environment(
    gate: GateSpec,
    authority_mode: str = AUTHORITY_MODE_LIVE,
) -> dict[str, str]:
    if authority_mode not in AUTHORITY_MODES:
        raise EvidenceError(f"gate {gate.id} received an invalid live-authority mode")
    allowlist = list(gate.environment_allowlist)
    if gate.id in LIVE_AUTHORITY_GATE_IDS and authority_mode == AUTHORITY_MODE_DIAGNOSTIC:
        allowlist.extend(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
    environment = {
        key: value
        for key in dict.fromkeys(allowlist)
        if (value := os.environ.get(key)) is not None
    }
    environment["CORTEX_GATE"] = "1"
    environment.update(GATE_FORCED_ENVIRONMENT_BY_ID.get(gate.id, {}))
    if gate.id in LIVE_AUTHORITY_GATE_IDS and authority_mode == AUTHORITY_MODE_LIVE:
        appdata, localappdata = _canonical_live_data_roots()
        environment["APPDATA"] = str(appdata)
        environment["LOCALAPPDATA"] = str(localappdata)
        leaked = sorted(set(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT) & set(environment))
        if leaked:
            raise EvidenceError(
                f"certifying live gate {gate.id} inherited caller authority: {', '.join(leaked)}"
            )
    environment.setdefault("PYTHONUTF8", "1")
    environment.setdefault("PYTHONIOENCODING", "utf-8")
    # Caller-controlled NODE_OPTIONS/PYTHONPATH/RUSTFLAGS can inject code or change discovery while
    # the underlying command still exits zero.  Certification workers receive only the verifier's
    # fatal-report option; those ambient mutation channels are deliberately absent from the allowlist.
    environment["NODE_OPTIONS"] = _node_report_opts
    return environment


def _gate_environment_for_id(
    gate_id: str,
    authority_mode: str = AUTHORITY_MODE_LIVE,
) -> dict[str, str]:
    """Return a production gate allowlist, or the secret-free base for compatibility diagnostics."""

    try:
        return _gate_environment(_gate_by_id(gate_id), authority_mode)
    except ValueError:
        environment = {
            key: value for key in GATE_BASE_ENVIRONMENT if (value := os.environ.get(key)) is not None
        }
        environment["CORTEX_GATE"] = "1"
        environment.setdefault("PYTHONUTF8", "1")
        environment.setdefault("PYTHONIOENCODING", "utf-8")
        environment["NODE_OPTIONS"] = _node_report_opts
        return environment


def _gate_environment_authority(
    gate: GateSpec,
    environment: dict[str, str],
    *,
    authority_mode: str,
    run_authority_digest: str,
) -> dict[str, object]:
    if authority_mode not in AUTHORITY_MODES or not re.fullmatch(
        r"[0-9a-f]{64}", run_authority_digest
    ):
        raise EvidenceError("gate environment has no valid run-authority identity")
    secret_names = {"GH_TOKEN", "GITHUB_TOKEN"}
    bindings: list[dict[str, object]] = []
    for name in sorted(environment):
        if name in secret_names:
            bindings.append({"name": name, "redactedSecretPresent": True})
        elif name in REDACTED_PATH_ENVIRONMENT:
            bindings.append(
                {"name": name, "pathSha256": _redacted_path_digest(Path(environment[name]))}
            )
        else:
            bindings.append(
                {
                    "name": name,
                    "valueSha256": hashlib.sha256(environment[name].encode("utf-8")).hexdigest(),
                }
            )
    document: dict[str, object] = {
        "schema": 1,
        "gateId": gate.id,
        "liveAuthorityGate": gate.id in LIVE_AUTHORITY_GATE_IDS,
        "authorityMode": authority_mode,
        "runAuthorityDigest": run_authority_digest,
        "environmentAllowlist": list(gate.environment_allowlist),
        "effectiveEnvironment": bindings,
    }
    return {**document, "environmentDigest": _document_digest(document)}


def _validate_gate_environment_authority(
    value: object,
    gate: GateSpec,
    *,
    authority_mode: str,
    run_authority_digest: str,
) -> dict[str, object]:
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise EvidenceError(f"gate {gate.id} has no environment-authority binding")
    if set(value) != {
        "schema",
        "gateId",
        "liveAuthorityGate",
        "authorityMode",
        "runAuthorityDigest",
        "environmentAllowlist",
        "effectiveEnvironment",
        "environmentDigest",
    }:
        raise EvidenceError(f"gate {gate.id} environment authority has a non-canonical envelope")
    digest = value.get("environmentDigest")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise EvidenceError(f"gate {gate.id} environment-authority digest is malformed")
    unsigned = {key: item for key, item in value.items() if key != "environmentDigest"}
    if _document_digest(unsigned) != digest:
        raise EvidenceError(f"gate {gate.id} environment-authority digest is invalid")
    if (
        value.get("gateId") != gate.id
        or value.get("liveAuthorityGate") is not (gate.id in LIVE_AUTHORITY_GATE_IDS)
        or value.get("authorityMode") != authority_mode
        or value.get("runAuthorityDigest") != run_authority_digest
        or value.get("environmentAllowlist") != list(gate.environment_allowlist)
    ):
        raise EvidenceError(f"gate {gate.id} environment authority was substituted")
    bindings = value.get("effectiveEnvironment")
    if not isinstance(bindings, list) or any(not isinstance(item, dict) for item in bindings):
        raise EvidenceError(f"gate {gate.id} effective environment is malformed")
    names = [item.get("name") for item in bindings]
    if any(not isinstance(name, str) for name in names) or names != sorted(set(names)):
        raise EvidenceError(f"gate {gate.id} effective environment keys are invalid")
    permitted = set(gate.environment_allowlist) | {
        "PYTHONUTF8",
        "PYTHONIOENCODING",
        "NODE_OPTIONS",
    }
    if gate.id in LIVE_AUTHORITY_GATE_IDS and authority_mode == AUTHORITY_MODE_DIAGNOSTIC:
        permitted.update(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT)
    if not set(names) <= permitted:
        raise EvidenceError(f"gate {gate.id} effective environment escaped its allowlist")
    if gate.id in LIVE_AUTHORITY_GATE_IDS and authority_mode == AUTHORITY_MODE_LIVE:
        leaked = sorted(set(names) & set(LIVE_AUTHORITY_OVERRIDE_ENVIRONMENT))
        if leaked:
            raise EvidenceError(
                f"certifying live gate {gate.id} contains caller authority: {', '.join(leaked)}"
            )
        if not {"APPDATA", "LOCALAPPDATA"} <= set(names):
            raise EvidenceError(f"certifying live gate {gate.id} omits Windows data roots")
    for item in bindings:
        if item.get("name") in {"GH_TOKEN", "GITHUB_TOKEN"}:
            if set(item) != {"name", "redactedSecretPresent"} or item.get(
                "redactedSecretPresent"
            ) is not True:
                raise EvidenceError(f"gate {gate.id} secret binding is not redacted")
        elif item.get("name") in REDACTED_PATH_ENVIRONMENT:
            if set(item) != {"name", "pathSha256"}:
                raise EvidenceError(f"gate {gate.id} environment path binding is not redacted")
            if not re.fullmatch(r"[0-9a-f]{64}", str(item.get("pathSha256", ""))):
                raise EvidenceError(f"gate {gate.id} environment path binding is malformed")
        elif set(item) != {"name", "valueSha256"} or not re.fullmatch(
            r"[0-9a-f]{64}", str(item.get("valueSha256", ""))
        ):
            raise EvidenceError(f"gate {gate.id} environment value binding is malformed")
    return value

# Charter DoD legs descoped by the owner amendment (2026-07-10) — always printed.
DESCOPED = [
    ("asosoft-600-eval-set", "Eval corpus: owner decision 2026-08-11 — AsoSoft publishes NO licence file and NO terms beyond \"research and non-commercial use\", and no contact address; evaluation rests on FLEURS ckb + CORDI (CC BY-SA 4.0) instead"),
    ("store-install-paths", "Distribution: winget / Homebrew / Flathub"),
    ("hf-model-card", "Distribution: HF card + eval YAML + ethics section"),
    ("macos-notarization", "Distribution (was the explicit STRETCH leg)"),
    ("openssf-scorecard-check", "Engineering rigor: Scorecard >=8.0 required check"),
]

# Kept, mandatory for full 10/10, waiting on the human — always printed.
OWNER_GATED = [
    ("iaa-kappa-ceiling", "item 44: recruit >=2 independent Sorani annotators"),
    ("cordi-dialect-fairness", "item 53: CORDI corpus agreement"),
    ("refinery-lift-in-product", "item 37: Gold Marathon (>=500 real review decisions)"),
]


LOG_DIR = Path(tempfile.gettempdir()) / "cortex-verify10"
PROOF_ROOT = LOG_DIR / "proof-runs"
LATEST_PROOF = LOG_DIR / "latest-proof.json"
RUN_LOCK = LOG_DIR / "verify10.lease.json"
RUST_COVERAGE_PHASE_ROOT = LOG_DIR / "rust-coverage-prerequisites"
RUST_COVERAGE_LATEST = LOG_DIR / "latest-rust-coverage-prerequisite.json"
RUST_COVERAGE_LOCK = LOG_DIR / "rust-coverage-prerequisite.lease.json"
RUST_COVERAGE_PHASE_DIRNAME = "rust-coverage-prerequisite"
RUST_COVERAGE_ARTIFACT_NAME = "rust-coverage.llvm.json"
RUST_COVERAGE_MANIFEST_NAME = "rust-coverage-manifest.json"
RUST_COVERAGE_TOOLCHAIN_CONTRACT = APP / "scripts" / "rust_coverage_toolchain.json"
RUST_COVERAGE_INNER_TIMEOUT_SECONDS = 7_200
RUST_COVERAGE_SUPERVISOR_TIMEOUT_SECONDS = 7_500
RUST_COVERAGE_FRESH_SECONDS = 8 * 60 * 60

# Append-only per-gate run record (external review 2026-08-06, P0.1): "a result that ran but cannot be
# retrieved is operationally indistinguishable from no result."
#
# The summary table is printed only after the LAST gate, so a caller that gave a ~40-minute sweep a
# ~30-minute timeout threw away every gate that had already passed — the work was done and the evidence
# was not. This file is written as each gate FINISHES, so a killed, timed-out or crashed run still leaves
# a durable, ordered record of exactly how far it got and what each leg cost.
#
# JSONL and append-only on purpose: a partial last line is the only damage a kill can do, and every line
# before it stays parseable. Evidence is part of the verdict: a write/fsync failure fails the verifier.
RUN_LOG = LOG_DIR / "runs.jsonl"


def record_run_event(**fields):
    """Append one JSON line to the legacy aggregate log and fail closed on evidence loss."""
    try:
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        with RUN_LOG.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(fields, ensure_ascii=False) + "\n")
            fh.flush()
            os.fsync(fh.fileno())  # a record that is still in a buffer when the process is killed is not a record
    except OSError as exc:
        raise EvidenceError(f"run-log unavailable: {exc}") from exc

# Everything this script runs is THE GATE, and a gate must never quietly reuse a resource it did not
# create. Set for the whole run (subprocesses inherit it) rather than per leg, because that is exactly
# what it means: any harness that can attach to somebody else's server, browser or port should refuse
# when it sees this and say why.
#
# First consumer: playwright.config.ts, whose `reuseExistingServer` was TRUE locally. DEMONSTRATED
# 2026-08-03 — an impostor server placed on port 1420 was silently reused and the accessibility spec
# ran against "not the app". A foreign server makes the leg red; a STALE but valid dev server makes it
# GREEN about code that is not under test.
os.environ["CORTEX_GATE"] = "1"

# CAPTURE THE NEXT 0xC0000409 INSTEAD OF LOSING IT.
#
# Twice now a Node leg has died with exit 3221226505 (STATUS_STACK_BUFFER_OVERRUN) inside a full sweep
# and NEVER standalone (43 clean runs): `heartbeat-runtime` at 4.7s with its first line printed, and
# `finetuned-ipc-e2e` at 0.6s with stdout completely EMPTY — dead before its own first console.log, i.e.
# during Node/V8 startup or module load, not in the test body. stderr was empty both times and Windows
# Error Reporting logged nothing, so there has been nothing to diagnose from.
#
# `--report-on-fatalerror` makes Node write a JSON diagnostic report (native + JS stacks, heap and
# resource-usage counters, loaded libraries, the OS error) when V8 or the runtime dies fatally — which
# is exactly the class 0xC0000409 belongs to, since a CRT/V8 abort() on Windows surfaces as fastfail.
# Costs nothing on a healthy run: no report is written unless the process dies fatally.
#
# STANCE CHANGED 2026-08-05, on evidence rather than convenience. This said "deliberately NOT a
# retry" when the cause was unknown and the crash might have been the app dying. It is not: the
# process that exits 3221226505 is node.exe, the harness, and it dies BEFORE the probe measures
# anything (phase markers put the two heartbeat deaths inside the 8.2s debug-port wait). A leg that
# produced no measurement is not evidence that the app failed its gate, so reporting it as a red gate
# was itself a false claim.
#
# `run_gate` now re-runs ONCE on ABNORMAL_EXIT_CODES and stamps a `<gate>.CRASH.<ts>.log` for the dead
# attempt first, so the occurrence stays counted even when the retry passes. The report flag below
# stays: it costs nothing, and it did NOT fire on the 2026-08-05 crash — which is itself the finding
# that ruled out a V8/CRT abort Node could intercept.
# Exit codes that mean "the OS killed this process", NOT "this test failed". A failing gate exits 1
# (or its own small code); these are NTSTATUS values surfaced as a process exit code, so the leg
# produced no verdict at all. Only the one actually observed is listed — adding speculative codes
# would widen a retry path on no evidence.
ABNORMAL_EXIT_CODES = frozenset({3221226505})  # 0xC0000409 STATUS_STACK_BUFFER_OVERRUN

LOG_DIR.mkdir(parents=True, exist_ok=True)
_node_report_opts = f"--report-on-fatalerror --report-directory={LOG_DIR}"
os.environ["NODE_OPTIONS"] = (os.environ.get("NODE_OPTIONS", "") + " " + _node_report_opts).strip()


def _retired_captured_run_gate(name, kind, payload, cwd, probe, timeout=3600):
    """Run one gate; returns (status, seconds, detail). Full cmd output -> LOG_DIR/<gate>.log.

    A probe returns None (runnable), a reason string (SKIP-ENV: the environment cannot run this leg),
    or an explicit (status, reason) pair when what it found is a real defect rather than missing
    machine state — see `_probe_champion_7b`.
    """
    if probe:
        try:
            verdict = probe()
        except Exception as e:  # noqa: BLE001 - one broken probe must not abort the remaining gates
            return FAIL, 0.0, f"probe crashed: {e}"
        if verdict:
            status, reason = verdict if isinstance(verdict, tuple) else (SKIP_ENV, verdict)
            return status, 0.0, reason
    if kind == "not-built":
        return NOT_BUILT, 0.0, payload
    t0 = time.perf_counter()
    if kind == "fn":
        try:
            ok = payload()
        except Exception as e:  # noqa: BLE001 - a crashing gate is a red gate
            return FAIL, time.perf_counter() - t0, f"gate crashed: {e}"
        return (PASS if ok else FAIL), time.perf_counter() - t0, ""
    # kind == "cmd"
    retried = ""
    first_attempt = ""
    try:
        r = subprocess.run(
            _command_argv(payload), shell=False, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
        # LNK1104 on system libs is a Windows file-lock (AV scan) flake, not a code failure:
        # retry exactly once, and say so — both attempts land in the log. Keeping the first one is
        # the point: without it a repeat flake is invisible, and the retry's own output cannot be
        # compared against what it replaced.
        if r.returncode != 0 and "LNK1104" in (r.stdout or "") + (r.stderr or ""):
            retried = " [retried once after LNK1104 linker file-lock flake]"
            first_attempt = (
                f"--- first attempt (exit {r.returncode}, LNK1104 linker file-lock flake) ---\n"
                f"{r.stdout or ''}\n{r.stderr or ''}\n\n"
            )
            r = subprocess.run(
                _command_argv(payload), shell=False, cwd=cwd, capture_output=True, text=True, timeout=timeout
            )
    except subprocess.TimeoutExpired:
        return FAIL, time.perf_counter() - t0, f"timed out after {timeout}s"
    secs = time.perf_counter() - t0
    # Persist the FULL output so every failure stays diagnosable after the run.
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{name}.log"
    log_path.write_text(
        f"$ {payload}\n(exit {r.returncode}, {secs:.1f}s){retried}\n\n{first_attempt}"
        f"--- stdout ---\n{r.stdout or ''}"
        f"\n--- stderr ---\n{r.stderr or ''}",
        encoding="utf-8",
        errors="replace",
    )
    # OS-LEVEL ABNORMAL TERMINATION — not a test failure, and not a verdict about the app.
    #
    # Measured three times (2026-08-03 heartbeat-runtime 4.7s, 2026-08-04 finetuned-ipc-e2e 0.6s,
    # 2026-08-05 heartbeat-runtime 6.7s): a Node probe process died with exit 3221226505
    # (0xC0000409 STATUS_STACK_BUFFER_OVERRUN) ONLY inside a full sweep, never in 103 standalone runs,
    # always before it measured anything. The process that dies is node.exe — the harness — not the
    # app under test. `--report-on-fatalerror` wrote no report and Windows Error Reporting logged
    # nothing, which is consistent with a native fastfail that bypasses both.
    #
    # Reporting that as "the app failed its responsiveness gate" is a FALSE CLAIM: there is no
    # measurement to fail. So re-run once — exactly like the LNK1104 branch above — but stamp a
    # CRASH copy of the dead attempt FIRST, so an occurrence can never become invisible merely
    # because the retry passed. That preservation is the point: the crash stays counted.
    #
    # Deliberately narrow. A probe that RAN and exceeded its threshold exits 1 and never reaches
    # here, so this cannot turn a real regression green.
    if r.returncode in ABNORMAL_EXIT_CODES and not retried:
        crash_log = LOG_DIR / f"{name}.CRASH.{time.strftime('%Y%m%d-%H%M%S')}.log"
        try:
            shutil.copyfile(log_path, crash_log)
        except OSError as e:  # bookkeeping must never turn a diagnosable crash into a gate crash
            print(f"  (could not keep the crash log: {e})", flush=True)
            crash_log = None
        print(
            f"  !! {name}: harness process terminated by the OS (exit {r.returncode}) before producing"
            f" any verdict — re-running once. Evidence kept: {crash_log}",
            flush=True,
        )
        retried = f" [OS-terminated (exit {r.returncode}) with no verdict; re-ran once — see {crash_log}]"
        try:
            r = subprocess.run(
                _command_argv(payload), shell=False, cwd=cwd, capture_output=True, text=True, timeout=timeout
            )
        except subprocess.TimeoutExpired:
            return FAIL, time.perf_counter() - t0, f"timed out after {timeout}s (on the post-crash re-run)"
        secs = time.perf_counter() - t0
        log_path.write_text(
            f"$ {payload}\n(exit {r.returncode}, {secs:.1f}s){retried}\n\n--- stdout ---{r.stdout or ''}"
            f"\n--- stderr ---\n{r.stderr or ''}",
            encoding="utf-8",
            errors="replace",
        )

    if r.returncode == 0:
        return PASS, secs, retried.strip()
    # A FAILURE ALSO GETS A TIMESTAMPED COPY, because the line above only keeps the LATEST run of each
    # gate. That is fine for a failure you investigate immediately and useless for the failure that
    # matters most: the INTERMITTENT one. Measured 2026-08-03 — `test-e2e+a11y` crashed with exit
    # 3221226505 (0xC0000409, stack buffer overrun) in one sweep of three, and by the time it could be
    # read the next sweep had already overwritten the log with a passing run. The evidence for the only
    # unexplained fault of the night was destroyed by the gate's own success.
    #
    # Copy, not move: the stable `<gate>.log` path is what the FAIL line prints and what people already
    # look for, so it keeps meaning "the most recent run".
    stamped = LOG_DIR / f"{name}.FAIL.{time.strftime('%Y%m%d-%H%M%S')}.log"
    try:
        shutil.copyfile(log_path, stamped)
    except OSError as e:  # never let bookkeeping turn a diagnosable failure into a crash
        print(f"  (could not keep a timestamped copy of the failure log: {e})", flush=True)
        stamped = None
    tail = "\n".join(((r.stdout or "") + "\n" + (r.stderr or "")).strip().splitlines()[-12:])
    kept = f"\n     kept for post-mortem: {stamped}" if stamped else ""
    return FAIL, secs, f"exit {r.returncode}{retried} - full log: {log_path}{kept}\n{tail}"


def _attempt_log_path(name: str, attempt: int) -> Path:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    return LOG_DIR / f"{name}.attempt-{attempt}.{stamp}.{uuid.uuid4().hex[:8]}.log"


def _run_command_attempt(
    name: str,
    steps: tuple[GateStep, ...],
    cwd: Path,
    timeout: int,
    attempt: int,
    authority_mode: str = AUTHORITY_MODE_LIVE,
) -> tuple[int | None, float, Path, bool]:
    """Run explicit substeps into one durable attempt log and kill every descendant on timeout."""

    if timeout <= 0:
        raise ValueError(f"gate {name} has no positive timeout")
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = _attempt_log_path(name, attempt)
    started = time.perf_counter()
    deadline = started + timeout
    return_code: int | None = 0
    timed_out = False
    try:
        with log_path.open("x", encoding="utf-8", errors="replace", buffering=1) as log:
            for index, step in enumerate(steps, start=1):
                log.write(
                    f"--- substep {index}/{len(steps)} ---\n"
                    f"argv={json.dumps(list(step.argv), ensure_ascii=False)}\n"
                    f"cwd={cwd}\n\n"
                )
                log.flush()
                os.fsync(log.fileno())
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    timed_out = True
                    return_code = None
                    break
                process, job = spawn_isolated(
                    list(step.argv),
                    cwd=cwd,
                    log=log,
                    env=_gate_environment_for_id(name, authority_mode),
                )
                return_code, step_timeout = wait_isolated(
                    process,
                    job,
                    timeout=remaining,
                    heartbeat=lambda: None,
                )
                log.write(f"\n--- substep exit {return_code} ---\n")
                log.flush()
                os.fsync(log.fileno())
                if step_timeout:
                    timed_out = True
                    break
                if return_code != 0:
                    break
    except OSError as error:
        raise EvidenceError(f"cannot stream gate {name} attempt {attempt} evidence: {error}") from error
    return return_code, time.perf_counter() - started, log_path, timed_out


def _publish_latest_gate_log(name: str, attempt_logs: list[Path]) -> Path:
    chunks: list[bytes] = []
    for index, path in enumerate(attempt_logs, start=1):
        chunks.append(f"===== ATTEMPT {index}: {path.name} =====\n".encode("utf-8"))
        chunks.append(path.read_bytes())
        chunks.append(b"\n")
    latest = LOG_DIR / f"{name}.log"
    atomic_write_bytes(latest, b"".join(chunks))
    return latest


def run_gate(
    name,
    kind,
    payload,
    cwd,
    probe,
    timeout=3600,
    authority_mode=AUTHORITY_MODE_LIVE,
):
    """Compatibility entrypoint backed by explicit argv, durable logs and process-tree cleanup."""

    if probe:
        try:
            verdict = probe()
        except Exception as error:  # noqa: BLE001 - a broken probe is this gate's failure
            return FAIL, 0.0, f"probe crashed: {error}"
        if verdict:
            status, reason = verdict if isinstance(verdict, tuple) else (SKIP_ENV, verdict)
            return status, 0.0, reason
    if kind == "not-built":
        return NOT_BUILT, 0.0, payload
    started = time.perf_counter()
    if kind == "fn":
        try:
            ok = payload()
        except Exception as error:  # noqa: BLE001 - a crashing worker is a red gate
            return FAIL, time.perf_counter() - started, f"gate crashed: {error}"
        return (PASS if ok else FAIL), time.perf_counter() - started, ""
    if kind != "cmd" or not isinstance(payload, str):
        return FAIL, 0.0, f"invalid gate kind/payload: {kind!r}"

    try:
        steps = _command_steps(payload)
        return_code, seconds, first_log, timed_out = _run_command_attempt(
            name, steps, Path(cwd or REPO_ROOT), timeout, 1, authority_mode
        )
    except (OSError, ValueError, EvidenceError) as error:
        return FAIL, time.perf_counter() - started, f"gate supervisor failed: {error}"
    attempt_logs = [first_log]
    if timed_out:
        latest = _publish_latest_gate_log(name, attempt_logs)
        return FAIL, seconds, f"timed out after {timeout}s; full log: {latest}"

    first_output = first_log.read_text(encoding="utf-8", errors="replace")
    retry_reason: str | None = None
    if return_code != 0 and "LNK1104" in first_output:
        retry_reason = "LNK1104 linker file-lock flake"
    elif return_code in ABNORMAL_EXIT_CODES:
        retry_reason = f"OS-terminated before verdict (exit {return_code})"

    if retry_reason is not None:
        try:
            retry_code, retry_seconds, retry_log, retry_timed_out = _run_command_attempt(
                name, steps, Path(cwd or REPO_ROOT), timeout, 2, authority_mode
            )
            attempt_logs.append(retry_log)
            seconds += retry_seconds
        except (OSError, ValueError, EvidenceError) as error:
            latest = _publish_latest_gate_log(name, attempt_logs)
            return FAIL, time.perf_counter() - started, (
                f"{retry_reason}; diagnostic re-run could not start: {error}; full log: {latest}"
            )
        latest = _publish_latest_gate_log(name, attempt_logs)
        if retry_timed_out:
            return FAIL, seconds, f"{retry_reason}; re-run timed out; full log: {latest}"
        if retry_code == 0:
            return PASS_AFTER_RETRY, seconds, f"{retry_reason}; re-ran once; full log: {latest}"
        return FAIL, seconds, (
            f"{retry_reason}; re-run exit {retry_code}; full log: {latest}\n"
            + "\n".join(retry_log.read_text(encoding="utf-8", errors="replace").splitlines()[-12:])
        )

    latest = _publish_latest_gate_log(name, attempt_logs)
    if return_code == 0:
        return PASS, seconds, ""
    stamped = LOG_DIR / f"{name}.FAIL.{time.strftime('%Y%m%d-%H%M%S')}.{uuid.uuid4().hex[:8]}.log"
    atomic_write_bytes(stamped, latest.read_bytes())
    tail = "\n".join(first_output.strip().splitlines()[-12:])
    return FAIL, seconds, (
        f"exit {return_code}; full log: {latest}\n"
        f"     kept for post-mortem: {stamped}\n{tail}"
    )


def write_status_md(
    path,
    head,
    quick,
    results,
    verdict,
    profile=PROFILE_FULL,
    evidence_results: list[dict[str, object]] | None = None,
):
    """Emit the proof-local generated view of the completed gate results.

    Hand-written docs that restate which gates pass go stale silently — OWNER_HANDOFF.md
    claimed `egress-runtime` was NOT-BUILT for weeks after it shipped, and refinery-lift
    needed the same manual correction. Under this repo's honesty law a doc asserting a
    gate state it did not measure is exactly the failure mode to design out, so docs now
    link here instead of restating.

    It lives inside the immutable run directory and is hash-listed by ``manifest.json``. A tracked
    generated status cannot describe its own commit: committing the embedded SHA creates a different
    SHA. The repository therefore keeps only a static authority notice.
    """
    rows = "\n".join(f"| `{name}` | {status} |" for name, status, _, _ in results)
    descoped = "\n".join(f"| `{name}` | {why} |" for name, why in DESCOPED)
    gated = "\n".join(f"| `{name}` | {why} |" for name, why in OWNER_GATED)
    evidence_rows = evidence_results or list(_pending_evidence_results(profile))
    evidence_table = "\n".join(
        f"| `{item['classId']}` | {item['status']} | {item['detail']} |"
        for item in evidence_rows
    )
    body = f"""<!-- GENERATED inside an immutable proof run by scripts/verify_10.py. Do not edit. -->

# Gate status — generated

**Commit:** `{head}` · **Profile:** `{profile}` · **Mode:** {'quick (tiers 0-1)' if quick else 'full'}

**Verdict:** {verdict}

## Kept gates

| Gate | Status |
|---|---|
{rows}

## Required certification evidence

Only class-specific validator artifacts can produce `VERIFIED`. No user-authored pass flag is accepted.

| Evidence class | Status | Required proof / result |
|---|---|---|
{evidence_table}

## Outside the selected certification contract (never counted as a pass)

| Leg | Reason |
|---|---|
{descoped}

## Historical model-evidence dependencies (also represented above where applicable)

| Leg | Blocked on |
|---|---|
{gated}
"""
    atomic_write_bytes(Path(path), body.encode("utf-8"))
    print(f"\n[status-md] wrote {path}")


def _retired_aggregate_main(quick, status_md=None):
    raise EvidenceError(
        "the legacy aggregate path is retired because it has no typed profile, evidence contract, "
        "or ProductAttestationV1 and therefore cannot issue a release verdict"
    )
    head = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True
    ).stdout.strip() or "?"
    run_id = f"{time.strftime('%Y%m%dT%H%M%S')}-{head}"
    print("=" * 68)
    print(" CORTEX VERIFY-10 - PERSONAL-USE FULL-CHARTER GATE")
    print(f" repo: {REPO_ROOT}   HEAD: {head}   mode: {'quick (tiers 0-1)' if quick else 'full'}")
    print(f" per-gate logs: {LOG_DIR}")
    print(f" run record:    {RUN_LOG}   (run_id {run_id})")
    print("=" * 68)
    record_run_event(
        run_id=run_id,
        event="run_start",
        at=time.strftime("%Y-%m-%dT%H:%M:%S"),
        commit=head,
        mode="quick" if quick else "full",
        platform=sys.platform,
        python=sys.version.split()[0],
        gates_planned=len(GATES),
    )

    results = []
    for name, tier, kind, payload, cwd, probe, charter in GATES:
        if quick and tier > 1:
            results.append((name, NOT_RUN_QUICK, 0.0, ""))
            continue
        print(f"\n----- [tier {tier}] {name} :: {charter}")
        status, secs, detail = run_gate(name, kind, payload, cwd, probe)
        results.append((name, status, secs, detail))
        line = f"  => {status}   {name}   {secs:.1f}s"
        print(line if not detail else f"{line}\n     {detail}")
        # Written HERE, not with the summary below: this is the whole point. A sweep killed at gate 20
        # must still be able to prove what gates 1-19 did.
        record_run_event(
            run_id=run_id,
            event="gate",
            at=time.strftime("%Y-%m-%dT%H:%M:%S"),
            commit=head,
            gate=name,
            tier=tier,
            status=status,
            seconds=round(secs, 2),
            detail=detail,
            log=str(LOG_DIR / f"{name}.log"),
        )

    print("\n" + "-" * 68)
    for name, status, secs, detail in results:
        print(f" {status:<10} {name:<24} {secs:>7.1f}s")
    for name, why in DESCOPED:
        print(f" {'SKIPPED-BY-OWNER-DECISION':<26} {name:<28} ({why}; owner amendment 2026-07-10)")
    for name, why in OWNER_GATED:
        print(f" {'OWNER-GATED-PENDING':<26} {name:<28} ({why})")

    fails = [n for n, s, _, _ in results if s == FAIL]
    skips = [n for n, s, _, _ in results if s in (SKIP_ENV, NOT_BUILT, NOT_RUN_QUICK)]
    passes = [n for n, s, _, _ in results if s == PASS]
    print("-" * 68)
    print(
        f" kept gates run: {len(results)} - {len(passes)} PASS, {len(fails)} FAIL, "
        f"{len(skips)} skipped (env/not-built)"
    )
    print(f" owner-descoped: {len(DESCOPED)}   owner-gated pending: {len(OWNER_GATED)}")

    # Verdict + exit code are computed once and shared by stdout and the generated status
    # file, so the two can never disagree (a status doc contradicting the run that produced
    # it would be the same dishonesty this file exists to prevent). Exit codes are the CI
    # contract and are unchanged: 1 = RED, 2 = INCOMPLETE, 0 = green.
    if fails:
        verdict = f"**RED** — {len(fails)} kept gate(s) failed ({', '.join(fails)}). NOT ship-ready."
        code = 1
        console = f" VERDICT: RED - {len(fails)} kept gate(s) failed ({', '.join(fails)}). NOT ship-ready."
    elif skips:
        verdict = (
            f"**INCOMPLETE** — {len(skips)} kept gate(s) could not run "
            f"({', '.join(skips)}). Green cannot be claimed."
        )
        code = 2
        console = (
            f" VERDICT: INCOMPLETE - {len(skips)} kept gate(s) could not run "
            f"({', '.join(skips)}). Green cannot be claimed."
        )
    elif DESCOPED or OWNER_GATED:
        verdict = (
            "**GREEN — PERSONAL-USE SHIP-READY.** "
            f"(Not full-charter 10/10: {len(DESCOPED)} legs owner-descoped, "
            f"{len(OWNER_GATED)} owner-gated pending.)"
        )
        code = 0
        console = (
            " VERDICT: GREEN - PERSONAL-USE SHIP-READY. "
            f"(Not full-charter 10/10: {len(DESCOPED)} legs owner-descoped, "
            f"{len(OWNER_GATED)} owner-gated pending.)"
        )
    else:
        verdict = "**CORTEX 10/10: ALL GATES GREEN**"
        code = 0
        console = "CORTEX 10/10: ALL GATES GREEN"

    print(console)
    record_run_event(
        run_id=run_id,
        event="run_end",
        at=time.strftime("%Y-%m-%dT%H:%M:%S"),
        commit=head,
        exit_code=code,
        verdict=console,
        passed=len(passes),
        failed=fails,
        skipped=skips,
        total_seconds=round(sum(s for _, _, s, _ in results), 1),
        status_md=str(status_md) if status_md else None,
    )
    if status_md:
        write_status_md(status_md, head, quick, results, verdict)
    sys.exit(code)


PROFILE_REQUIRED_EVIDENCE: dict[str, tuple[str, ...]] = {
    profile: tuple(spec.id for spec in _required_evidence_specs(profile))
    for profile in (PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_MODEL, PROFILE_FULL)
}

MIGRATION_CATALOG = APP / "src-tauri" / "src" / "migrations" / "mod.rs"
MODEL_ATTESTATION = REPO_ROOT / "docs" / "eval" / "current-model-attestation.json"
PRODUCT_ATTESTATION_NAME = "product-attestation.json"
EVIDENCE_CONTRACT_NAME = "evidence-contract.json"
WINDOWS_RELEASE_AUTHORITY_NAME = "windows-release-bundle-authority.json"


def _git_blob_id(full_sha: str, relative_path: str) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", f"{full_sha}:{relative_path}"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    blob = completed.stdout.strip().casefold()
    if completed.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", blob):
        raise EvidenceError(f"cannot bind {relative_path} to the manifest commit")
    return blob


def _git_file_bytes(full_sha: str, relative_path: str) -> bytes:
    completed = subprocess.run(
        ["git", "show", f"{full_sha}:{relative_path}"],
        cwd=REPO_ROOT,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise EvidenceError(f"cannot read committed source authority {relative_path}")
    return completed.stdout


def _migration_history(source: bytes) -> list[tuple[int, str]]:
    try:
        text = source.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("migration catalog is not UTF-8") from error
    migrations = [
        (int(version), description)
        for version, description in re.findall(
            r'Migration\s*\{\s*version:\s*(\d+)\s*,\s*description:\s*"([^"]*)"', text
        )
    ]
    if not migrations:
        raise EvidenceError("migration catalog contains no migrations")
    versions = [version for version, _ in migrations]
    if len(versions) != len(set(versions)) or versions != list(range(1, max(versions) + 1)):
        raise EvidenceError("migration catalog is not unique and contiguous from version 1")
    return migrations


def _schema_authority_document(full_sha: str, working_bytes: bytes | None = None) -> dict[str, object]:
    relative = MIGRATION_CATALOG.relative_to(REPO_ROOT).as_posix()
    working_bytes = MIGRATION_CATALOG.read_bytes() if working_bytes is None else working_bytes
    committed_bytes = _git_file_bytes(full_sha, relative)
    history = _migration_history(working_bytes)
    committed_history = _migration_history(committed_bytes)
    canonical_history = json.dumps(history, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    canonical_committed_history = json.dumps(
        committed_history, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")
    return {
        "schema": 1,
        "catalogPath": relative,
        "catalogProofPath": "source-authority/migrations.rs",
        "catalogGitBlobSha1": _git_blob_id(full_sha, relative),
        "catalogSha256": hashlib.sha256(working_bytes).hexdigest(),
        "committedCatalogSha256": hashlib.sha256(committed_bytes).hexdigest(),
        "matchesCommittedSource": working_bytes == committed_bytes,
        "latestVersion": history[-1][0],
        "migrationCount": len(history),
        "historySha256": hashlib.sha256(canonical_history).hexdigest(),
        "committedLatestVersion": committed_history[-1][0],
        "committedMigrationCount": len(committed_history),
        "committedHistorySha256": hashlib.sha256(canonical_committed_history).hexdigest(),
    }


def _binary_identity(path: Path) -> tuple[str, int, str | None]:
    digest = hashlib.sha256()
    tail = b""
    marker_value: str | None = None
    with path.open("rb") as handle:
        opened = os.fstat(handle.fileno())
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
            window = tail + chunk
            marker = re.search(rb"CORTEX_BUILD_SHA:([0-9a-fA-F]{7,40}|unknown)", window)
            if marker and marker_value is None:
                marker_value = marker.group(1).decode("ascii").casefold()
            tail = window[-96:]
        closed = os.fstat(handle.fileno())
    current = path.stat()
    opened_identity = (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns)
    if opened_identity != (closed.st_dev, closed.st_ino, closed.st_size, closed.st_mtime_ns) or opened_identity != (
        current.st_dev,
        current.st_ino,
        current.st_size,
        current.st_mtime_ns,
    ):
        raise EvidenceError("release executable changed while its identity was being captured")
    return digest.hexdigest(), opened.st_size, marker_value


def configure_windows_release_authority(
    bundle_dir: Path,
    *,
    expected_sha: str,
    expected_repository: str,
    expected_ref: str,
    expected_version: str,
    signer_thumbprint: str,
    signer_cert_sha256: str,
    require_windows_product: bool,
) -> dict[str, object]:
    """Independently measure one candidate bundle for proof production/consumption.

    The result is process-local and derived from the supplied bytes.  No JSON status file can set
    this value, and no bundle validation mutates the active release pointer or production database.
    """

    global _WINDOWS_RELEASE_AUTHORITY
    authority = validate_windows_release_bundle(
        bundle_dir,
        expected_sha=expected_sha,
        expected_repository=expected_repository,
        expected_ref=expected_ref,
        expected_version=expected_version,
        signer_thumbprint=signer_thumbprint,
        signer_cert_sha256=signer_cert_sha256,
        repo_root=REPO_ROOT,
        verify_authenticode=True,
        verify_provenance=True,
        require_windows_product=require_windows_product,
    )
    _WINDOWS_RELEASE_AUTHORITY = authority
    return authority


def _authority_release_artifacts(authority: object) -> list[dict[str, object]]:
    if not isinstance(authority, dict) or authority.get("schema") != 1:
        raise EvidenceError("Windows release authority has the wrong schema")
    source = authority.get("source")
    crypto = authority.get("cryptographicValidation")
    artifacts = authority.get("artifacts")
    if (
        authority.get("type") != "WindowsReleaseBundleAuthorityV1"
        or not isinstance(source, dict)
        or not isinstance(crypto, dict)
        or crypto.get("authenticodeAndTimestampVerified") is not True
        or crypto.get("githubSigstoreProvenanceVerified") is not True
        or not isinstance(artifacts, list)
        or not artifacts
        or any(not isinstance(artifact, dict) for artifact in artifacts)
    ):
        raise EvidenceError("Windows release authority lacks independent cryptographic validation")
    return artifacts


def _release_artifact_bindings(full_sha: str) -> list[dict[str, object]]:
    """Record only artifacts the local verifier actually observed; never infer signing."""

    executable = runtime_exe()
    if not executable.is_file():
        return []
    executable_sha, executable_bytes, marker = _binary_identity(executable)
    authority = "explicit-diagnostic" if os.environ.get("CORTEX_APP_EXE") else "build-output"
    pointer_sha: str | None = None
    active_release_git_sha: str | None = None
    appdata = os.environ.get("APPDATA")
    localappdata = os.environ.get("LOCALAPPDATA")
    if appdata and localappdata:
        pointer_path = Path(appdata) / "cortex-speech" / ACTIVE_RELEASE_POINTER
        if pointer_path.is_file():
            try:
                pointer_value = json.loads(pointer_path.read_text(encoding="utf-8"))
                release_root = Path(localappdata) / "CortexSpeech" / "private-production-releases"
                active_executable = validate_active_release_runtime(pointer_value, release_root)
                if (
                    active_executable.resolve() == executable.resolve()
                    and pointer_value.get("appSha256") == executable_sha
                ):
                    authority = "active-immutable-release"
                    pointer_sha = sha256_file(pointer_path)
                    active_release_git_sha = str(pointer_value.get("appGitSha"))
                elif active_executable.resolve() == executable.resolve():
                    authority = "invalid-active-release-pointer"
            except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
                # The executable-freshness/live gates carry the failure.  An invalid pointer must not
                # be promoted into an attested release identity merely because an exe still exists.
                authority = "invalid-active-release-pointer"
    active_binding = {
        "role": "application-executable",
        "name": executable.name,
        "sha256": executable_sha,
        "bytes": executable_bytes,
        "buildGitSha": marker,
        "matchesFullGitSha": marker == full_sha,
        "authority": authority,
        "activeReleasePointerSha256": pointer_sha,
        "activeReleaseGitSha": active_release_git_sha,
    }
    if _WINDOWS_RELEASE_AUTHORITY is None:
        return [active_binding]

    measured = _authority_release_artifacts(_WINDOWS_RELEASE_AUTHORITY)
    measured_app = next(
        (artifact for artifact in measured if artifact.get("role") == "application-executable"),
        None,
    )
    if measured_app is None:
        raise EvidenceError("Windows release authority omits the application executable")
    for field in ("name", "sha256", "bytes", "buildGitSha", "matchesFullGitSha"):
        if measured_app.get(field) != active_binding.get(field):
            raise EvidenceError(
                "Windows bundle executable is not the exact active immutable application binary"
            )
    # Preserve the live immutable-release authority while carrying every independently measured
    # bundle/signature field.  There is one role and one binary identity, never a duplicate claim.
    merged_app = {**measured_app, **active_binding}
    return [
        merged_app,
        *(artifact for artifact in measured if artifact.get("role") != "application-executable"),
    ]


def _model_attestation_binding() -> dict[str, object] | None:
    if not MODEL_ATTESTATION.is_file():
        return None
    return {
        "path": MODEL_ATTESTATION.relative_to(REPO_ROOT).as_posix(),
        "sha256": sha256_file(MODEL_ATTESTATION),
        "bytes": MODEL_ATTESTATION.stat().st_size,
    }


def _known_defect_digest(
    results: list[dict[str, object]], evidence_results: list[dict[str, object]]
) -> str:
    unresolved = {
        "nonPassGates": [
            {"gateId": result.get("gateId"), "status": result.get("status")}
            for result in results
            if result.get("status") != PASS
        ],
        "unresolvedEvidenceClasses": [
            result["classId"]
            for result in evidence_results
            if result.get("status") != "VERIFIED"
        ],
    }
    canonical = json.dumps(unresolved, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def _full_git_sha() -> str:
    completed = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    sha = completed.stdout.strip().casefold()
    if completed.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise EvidenceError("cannot bind verifier run to a full Git SHA")
    return sha


def _source_tree_digest() -> str:
    completed = subprocess.run(
        ["git", "rev-parse", "HEAD^{tree}"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    digest = completed.stdout.strip().casefold()
    if completed.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", digest):
        raise EvidenceError("cannot bind verifier run to the source tree digest")
    return digest


def _source_tree_digest_for_sha(full_sha: str) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", f"{full_sha}^{{tree}}"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    digest = completed.stdout.strip().casefold()
    if completed.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", digest):
        raise EvidenceError("cannot resolve the manifest commit tree")
    return digest


def _checkout_state_digest() -> str:
    """Hash every byte not already fixed by HEAD, including untracked files.

    ``HEAD^{tree}`` alone is constant while a gate edits the checkout. Certification records this
    digest at run start and rechecks it around every gate and before publication, so a mixed-source
    run cannot retain the original SHA merely because the commit itself did not move.
    """

    diff = subprocess.run(
        ["git", "diff", "--binary", "--no-ext-diff", "HEAD", "--"],
        cwd=REPO_ROOT,
        capture_output=True,
        check=False,
    )
    others = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"],
        cwd=REPO_ROOT,
        capture_output=True,
        check=False,
    )
    if diff.returncode != 0 or others.returncode != 0:
        raise EvidenceError("cannot fingerprint the working checkout")
    digest = hashlib.sha256()
    digest.update(b"tracked-diff\0")
    digest.update(diff.stdout)
    digest.update(b"\0untracked\0")
    untracked = sorted(path for path in others.stdout.split(b"\0") if path)
    for encoded in untracked:
        relative = Path(os.fsdecode(encoded))
        candidate = (REPO_ROOT / relative).resolve()
        try:
            candidate.relative_to(REPO_ROOT.resolve())
        except ValueError as error:
            raise EvidenceError(f"untracked source path escapes the repository: {relative}") from error
        try:
            content = candidate.read_bytes()
        except OSError as error:
            raise EvidenceError(f"cannot hash untracked source path {relative}: {error}") from error
        digest.update(encoded)
        digest.update(b"\0")
        digest.update(hashlib.sha256(content).digest())
    return digest.hexdigest()


def _assert_source_state(full_sha: str, tree_digest: str, checkout_digest: str) -> None:
    if _full_git_sha() != full_sha:
        raise EvidenceError("Git HEAD changed during verification")
    if _source_tree_digest() != tree_digest:
        raise EvidenceError("the committed source tree changed during verification")
    if _checkout_state_digest() != checkout_digest:
        raise EvidenceError("working-copy bytes changed during verification")


def _environment_document() -> dict[str, object]:
    return {
        "schema": 1,
        "platform": platform.platform(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "python": sys.version,
        "pythonExecutable": sys.executable,
        "processor": platform.processor(),
    }


_RUST_QUALITY_MODULE: object | None = None


def _rust_quality_module():
    """Load the committed semantic validator used by the standalone coverage phase."""

    global _RUST_QUALITY_MODULE
    if _RUST_QUALITY_MODULE is not None:
        return _RUST_QUALITY_MODULE
    source = APP / "scripts" / "rust_quality_gate.py"
    spec = importlib.util.spec_from_file_location("cortex_rust_quality_phase", source)
    if spec is None or spec.loader is None:
        raise EvidenceError("cannot load the committed Rust coverage validator")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as error:  # noqa: BLE001 - a broken validator is evidence failure
        raise EvidenceError(f"cannot execute the committed Rust coverage validator: {error}") from error
    _RUST_QUALITY_MODULE = module
    return module


def _parse_utc(value: object, label: str) -> datetime:
    if not isinstance(value, str) or not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", value):
        raise EvidenceError(f"{label} is not canonical UTC")
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError as error:
        raise EvidenceError(f"{label} is not a real UTC instant") from error


def _format_utc(value: datetime) -> str:
    return value.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _rust_coverage_command_registry() -> dict[str, object]:
    module = _rust_quality_module()
    try:
        contract = module.load_coverage_toolchain_contract(RUST_COVERAGE_TOOLCHAIN_CONTRACT)
    except Exception as error:  # noqa: BLE001 - a malformed authority is evidence failure
        raise EvidenceError(f"Rust coverage toolchain contract is invalid: {error}") from error
    quality_path = APP / "scripts" / "rust_quality_gate.py"
    relative_quality = quality_path.relative_to(REPO_ROOT).as_posix()
    relative_contract = RUST_COVERAGE_TOOLCHAIN_CONTRACT.relative_to(REPO_ROOT).as_posix()
    registry = {
        "schema": 1,
        "phase": "rust-coverage-prerequisite",
        "argvTemplate": [
            sys.executable,
            relative_quality,
            "coverage",
            "--output",
            f"{{PHASE_DIR}}/{RUST_COVERAGE_ARTIFACT_NAME}",
            "--timeout-seconds",
            str(RUST_COVERAGE_INNER_TIMEOUT_SECONDS),
        ],
        "cwd": APP.relative_to(REPO_ROOT).as_posix(),
        "supervisorTimeoutSeconds": RUST_COVERAGE_SUPERVISOR_TIMEOUT_SECONDS,
        "retryPolicy": "none",
        "environmentAllowlist": list(RUST_COVERAGE_ENVIRONMENT_ALLOWLIST),
        "measurementArgv": [
            "cargo",
            f"+{contract.toolchain}",
            "llvm-cov",
            "--locked",
            "--all-targets",
            "--all-features",
            "--branch",
            "--json",
            "--output-path",
            RUST_COVERAGE_ARTIFACT_NAME,
        ],
        "thresholds": {
            "lines": 85.0,
            "regions": 85.0,
            "functions": 80.0,
            "branches": 80.0,
        },
        "criticalDomainThresholds": {
            "lines": 95.0,
            "regions": 95.0,
            "branches": 90.0,
        },
        "criticalDomainPatterns": {
            name: list(patterns)
            for name, patterns in module.CRITICAL_COVERAGE_DOMAINS.items()
        },
        "qualityValidator": {
            "path": relative_quality,
            "sha256": sha256_file(quality_path),
        },
        "coverageToolchainContract": {
            "path": relative_contract,
            "sha256": sha256_file(RUST_COVERAGE_TOOLCHAIN_CONTRACT),
            "toolchain": contract.toolchain,
            "host": contract.host,
            "cargoLlvmCovVersion": contract.cargo_llvm_cov_version,
        },
    }
    canonical = json.dumps(registry, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {**registry, "registrySha256": hashlib.sha256(canonical).hexdigest()}


def _tool_identity(
    executable: str,
    argv: list[str],
    environment: dict[str, str],
    *,
    resolved_path: Path | None = None,
) -> dict[str, object]:
    resolved = shutil.which(executable) if resolved_path is None else str(resolved_path)
    if resolved is None:
        raise EvidenceError(f"coverage prerequisite tool is unavailable: {executable}")
    path = Path(resolved).resolve(strict=True)
    try:
        completed = subprocess.run(
            argv,
            cwd=APP,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
            shell=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError(f"coverage prerequisite tool probe failed for {executable}: {error}") from error
    version = ((completed.stdout or "") + "\n" + (completed.stderr or "")).strip()
    if completed.returncode != 0 or not version or len(version) > 4096:
        raise EvidenceError(f"coverage prerequisite tool version is unprovable: {executable}")
    return {
        "name": path.name,
        "sha256": sha256_file(path),
        "bytes": path.stat().st_size,
        "version": version,
    }


def _rustup_tool_identity(
    executable: str,
    toolchain: str,
    version_args: list[str],
    environment: dict[str, str],
) -> dict[str, object]:
    try:
        completed = subprocess.run(
            ["rustup", "which", "--toolchain", toolchain, executable],
            cwd=APP,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
            shell=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError(f"coverage prerequisite cannot resolve {toolchain} {executable}: {error}") from error
    resolved = (completed.stdout or "").strip()
    if completed.returncode != 0 or not resolved or "\n" in resolved or "\r" in resolved:
        raise EvidenceError(f"coverage prerequisite cannot resolve {toolchain} {executable}")
    path = Path(resolved).resolve(strict=True)
    return _tool_identity(
        executable,
        [str(path), *version_args],
        environment,
        resolved_path=path,
    )


def _rust_coverage_environment_document() -> dict[str, object]:
    environment = _rust_coverage_environment()
    python_path = Path(sys.executable).resolve(strict=True)
    module = _rust_quality_module()
    try:
        contract = module.load_coverage_toolchain_contract(RUST_COVERAGE_TOOLCHAIN_CONTRACT)
        coverage_identity = module.verify_coverage_toolchain(
            contract_path=RUST_COVERAGE_TOOLCHAIN_CONTRACT,
            environment=environment,
        )
    except Exception as error:  # noqa: BLE001 - a missing/mismatched nightly is evidence failure
        raise EvidenceError(f"Rust coverage toolchain identity is invalid: {error}") from error
    return {
        "schema": 1,
        "host": _environment_document(),
        "python": {
            "name": python_path.name,
            "sha256": sha256_file(python_path),
            "bytes": python_path.stat().st_size,
            "version": sys.version,
        },
        # The normal compiler remains the repository-pinned stable authority. Branch coverage is a
        # separately supervised measurement and gets its own date-pinned nightly executable IDs.
        "productionRustc": _tool_identity("rustc", ["rustc", "-Vv"], environment),
        "productionCargo": _tool_identity("cargo", ["cargo", "--version", "--verbose"], environment),
        "coverageToolchain": asdict(coverage_identity),
        "coverageRustc": _rustup_tool_identity("rustc", contract.toolchain, ["-Vv"], environment),
        "coverageCargo": _rustup_tool_identity(
            "cargo", contract.toolchain, ["--version", "--verbose"], environment
        ),
        "cargoLlvmCov": _tool_identity(
            "cargo-llvm-cov",
            ["cargo", f"+{contract.toolchain}", "llvm-cov", "--version"],
            environment,
        ),
        "networkPolicy": "CARGO_NET_OFFLINE=true inside rust_quality_gate.py; no toolchain install during proof",
    }


def _expected_rust_coverage_toolchain_identity() -> dict[str, object]:
    module = _rust_quality_module()
    try:
        return asdict(module.expected_coverage_toolchain_identity(RUST_COVERAGE_TOOLCHAIN_CONTRACT))
    except Exception as error:  # noqa: BLE001 - a malformed authority is evidence failure
        raise EvidenceError(f"Rust coverage toolchain contract is invalid: {error}") from error


def _rust_coverage_report(
    path: Path,
    *,
    coverage_toolchain: object | None = None,
) -> dict[str, object]:
    module = _rust_quality_module()
    try:
        verdict = module.validate_coverage_file(path)
        report = module._coverage_json(verdict)
    except Exception as error:  # noqa: BLE001 - includes the validator's GateError
        raise EvidenceError(f"Rust coverage artifact is not certifying evidence: {error}") from error
    if not isinstance(report, dict) or report.get("passed") is not True:
        failures = report.get("failures") if isinstance(report, dict) else None
        raise EvidenceError(f"Rust coverage thresholds did not pass: {failures}")
    expected_thresholds = _rust_coverage_command_registry()["thresholds"]
    metrics = report.get("metrics")
    if not isinstance(metrics, dict) or set(metrics) != set(expected_thresholds):
        raise EvidenceError("Rust coverage report has incomplete metrics")
    for name, threshold in expected_thresholds.items():
        metric = metrics.get(name)
        if (
            not isinstance(metric, dict)
            or metric.get("required_percent") != threshold
            or not isinstance(metric.get("count"), int)
            or isinstance(metric.get("count"), bool)
            or metric.get("count", 0) <= 0
            or not isinstance(metric.get("covered"), int)
            or isinstance(metric.get("covered"), bool)
        ):
            raise EvidenceError(f"Rust coverage metric {name} is incomplete or threshold-substituted")
    critical_domains = report.get("criticalDomains")
    expected_domains = _rust_coverage_command_registry()["criticalDomainPatterns"]
    critical_thresholds = _rust_coverage_command_registry()["criticalDomainThresholds"]
    if not isinstance(critical_domains, dict) or set(critical_domains) != set(expected_domains):
        raise EvidenceError("Rust coverage report has incomplete critical-domain evidence")
    for domain_name, expected_patterns in expected_domains.items():
        domain = critical_domains.get(domain_name)
        if (
            not isinstance(domain, dict)
            or domain.get("passed") is not True
            or domain.get("patterns") != expected_patterns
            or not isinstance(domain.get("matchedFiles"), list)
            or not domain["matchedFiles"]
            or not all(isinstance(path, str) and path for path in domain["matchedFiles"])
        ):
            raise EvidenceError(
                f"Rust coverage critical domain {domain_name} is incomplete or substituted"
            )
        domain_metrics = domain.get("metrics")
        if not isinstance(domain_metrics, dict) or set(domain_metrics) != set(critical_thresholds):
            raise EvidenceError(f"Rust coverage critical domain {domain_name} has incomplete metrics")
        for metric_name, threshold in critical_thresholds.items():
            metric = domain_metrics.get(metric_name)
            if (
                not isinstance(metric, dict)
                or metric.get("required_percent") != threshold
                or not isinstance(metric.get("count"), int)
                or isinstance(metric.get("count"), bool)
                or metric.get("count", 0) <= 0
                or not isinstance(metric.get("covered"), int)
                or isinstance(metric.get("covered"), bool)
            ):
                raise EvidenceError(
                    f"Rust coverage critical domain {domain_name} metric {metric_name} "
                    "is incomplete or threshold-substituted"
                )
    if coverage_toolchain is not None:
        if coverage_toolchain != _expected_rust_coverage_toolchain_identity():
            raise EvidenceError("Rust coverage report names an unapproved measurement toolchain")
        report["toolchain"] = coverage_toolchain
    return report


def _rust_coverage_phase_artifacts(phase_dir: Path) -> list[dict[str, object]]:
    artifacts: list[dict[str, object]] = []
    if any(candidate.is_symlink() for candidate in phase_dir.rglob("*")):
        raise EvidenceError("Rust coverage phase contains a symlink")
    for path in sorted(candidate for candidate in phase_dir.rglob("*") if candidate.is_file()):
        if path.name == RUST_COVERAGE_MANIFEST_NAME and path.parent == phase_dir:
            continue
        if path.is_symlink():
            raise EvidenceError("Rust coverage phase contains a symlink")
        artifacts.append(
            {
                "path": path.relative_to(phase_dir).as_posix(),
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
            }
        )
    return artifacts


def _validate_rust_coverage_phase(
    manifest_path: Path,
    *,
    expected_sha: str,
    expected_checkout_digest: str | None,
    require_fresh: bool,
    require_current_environment: bool,
) -> dict[str, object]:
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"Rust coverage prerequisite manifest cannot be read: {error}") from error
    exact_fields = {
        "schema",
        "type",
        "complete",
        "runToken",
        "fullGitSha",
        "sourceTreeDigest",
        "checkoutStateDigest",
        "startedAt",
        "endedAt",
        "expiresAt",
        "exitCode",
        "attemptCount",
        "commandRegistry",
        "environment",
        "coverage",
        "artifacts",
    }
    if not isinstance(manifest, dict) or set(manifest) != exact_fields:
        raise EvidenceError("Rust coverage prerequisite manifest has a non-canonical envelope")
    token = manifest.get("runToken")
    phase_dir = manifest_path.parent.resolve()
    if (
        manifest_path.name != RUST_COVERAGE_MANIFEST_NAME
        or not isinstance(token, str)
        or not re.fullmatch(r"[0-9a-f]{32}", token)
        or phase_dir.name != token
        or manifest.get("schema") != 1
        or manifest.get("type") != "RustCoveragePrerequisiteV1"
        or manifest.get("complete") is not True
        or manifest.get("exitCode") != 0
        or manifest.get("attemptCount") != 1
    ):
        raise EvidenceError("Rust coverage prerequisite has invalid completion/run identity")
    if (
        manifest.get("fullGitSha") != expected_sha
        or manifest.get("sourceTreeDigest") != _source_tree_digest_for_sha(expected_sha)
        or not re.fullmatch(r"[0-9a-f]{64}", str(manifest.get("checkoutStateDigest", "")))
        or (
            expected_checkout_digest is not None
            and manifest.get("checkoutStateDigest") != expected_checkout_digest
        )
    ):
        raise EvidenceError("Rust coverage prerequisite is bound to another source state")
    started = _parse_utc(manifest.get("startedAt"), "coverage phase startedAt")
    ended = _parse_utc(manifest.get("endedAt"), "coverage phase endedAt")
    expires = _parse_utc(manifest.get("expiresAt"), "coverage phase expiresAt")
    if ended < started or expires != ended + timedelta(seconds=RUST_COVERAGE_FRESH_SECONDS):
        raise EvidenceError("Rust coverage prerequisite has invalid duration/freshness authority")
    now = datetime.now(timezone.utc)
    if ended > now + timedelta(minutes=5):
        raise EvidenceError("Rust coverage prerequisite completion time is in the future")
    if require_fresh and not now < expires:
        raise EvidenceError("Rust coverage prerequisite is stale")
    registry = _rust_coverage_command_registry()
    if manifest.get("commandRegistry") != registry:
        raise EvidenceError("Rust coverage prerequisite command registry was substituted")
    environment = manifest.get("environment")
    if not isinstance(environment, dict) or environment.get("schema") != 1:
        raise EvidenceError("Rust coverage prerequisite environment is malformed")
    coverage_toolchain = environment.get("coverageToolchain")
    if coverage_toolchain != _expected_rust_coverage_toolchain_identity():
        raise EvidenceError("Rust coverage prerequisite omits or substituted its exact nightly identity")
    if require_current_environment and environment != _rust_coverage_environment_document():
        raise EvidenceError("Rust coverage prerequisite toolchain/environment changed before consumption")

    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list):
        raise EvidenceError("Rust coverage prerequisite has no artifact inventory")
    artifact_by_path: dict[str, dict[str, object]] = {}
    if any(candidate.is_symlink() for candidate in phase_dir.rglob("*")):
        raise EvidenceError("Rust coverage prerequisite contains a symlink")
    for artifact in artifacts:
        if not isinstance(artifact, dict) or set(artifact) != {"path", "sha256", "bytes"}:
            raise EvidenceError("Rust coverage prerequisite has a malformed artifact identity")
        relative_value = artifact.get("path")
        relative = Path(str(relative_value))
        candidate = (phase_dir / relative).resolve()
        try:
            candidate.relative_to(phase_dir)
        except ValueError as error:
            raise EvidenceError("Rust coverage prerequisite artifact escapes its phase") from error
        size = artifact.get("bytes")
        if (
            not isinstance(relative_value, str)
            or relative.is_absolute()
            or str(relative) in {"", "."}
            or relative_value in artifact_by_path
            or not isinstance(size, int)
            or isinstance(size, bool)
            or size < 0
            or not candidate.is_file()
            or candidate.is_symlink()
            or candidate.stat().st_size != size
            or sha256_file(candidate) != artifact.get("sha256")
        ):
            raise EvidenceError("Rust coverage prerequisite artifact is unsafe, missing, or changed")
        artifact_by_path[relative_value] = artifact
    required = {RUST_COVERAGE_ARTIFACT_NAME, "events.jsonl", "worker.log"}
    if not required <= set(artifact_by_path):
        raise EvidenceError("Rust coverage prerequisite omits a required phase artifact")
    actual = {
        candidate.relative_to(phase_dir).as_posix()
        for candidate in phase_dir.rglob("*")
        if candidate.is_file() and candidate != manifest_path
    }
    if actual != set(artifact_by_path):
        raise EvidenceError("Rust coverage prerequisite artifact inventory is not exact")

    coverage_path = phase_dir / RUST_COVERAGE_ARTIFACT_NAME
    report = _rust_coverage_report(coverage_path, coverage_toolchain=coverage_toolchain)
    if manifest.get("coverage") != report:
        raise EvidenceError("Rust coverage prerequisite report was forged or substituted")
    if report.get("artifactSha256") != artifact_by_path[RUST_COVERAGE_ARTIFACT_NAME]["sha256"]:
        raise EvidenceError("Rust coverage report does not bind its LLVM artifact")

    try:
        events = [
            json.loads(line)
            for line in (phase_dir / "events.jsonl").read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"Rust coverage prerequisite journal cannot be read: {error}") from error
    if len(events) < 2:
        raise EvidenceError("Rust coverage prerequisite journal is incomplete")
    for sequence, event in enumerate(events, start=1):
        if (
            not isinstance(event, dict)
            or event.get("schema") != 1
            or event.get("sequence") != sequence
            or event.get("runToken") != token
        ):
            raise EvidenceError("Rust coverage prerequisite journal identity/sequence is invalid")
    first, last = events[0], events[-1]
    if (
        first.get("event") != "phase_start"
        or first.get("fullGitSha") != expected_sha
        or first.get("checkoutStateDigest") != manifest.get("checkoutStateDigest")
        or first.get("commandRegistryHash") != registry["registrySha256"]
        or first.get("at") != manifest.get("startedAt")
        or last.get("event") != "phase_end"
        or last.get("exitCode") != 0
        or last.get("artifactSha256") != report["artifactSha256"]
        or last.get("at") != manifest.get("endedAt")
        or any(event.get("event") in {"retry", "abandonment"} for event in events)
    ):
        raise EvidenceError("Rust coverage prerequisite journal does not prove one clean attempt")
    return manifest


def _validate_latest_rust_coverage_pointer(
    pointer_path: Path,
    *,
    expected_sha: str,
    expected_checkout_digest: str,
) -> dict[str, object]:
    try:
        pointer = json.loads(pointer_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"latest Rust coverage pointer cannot be read: {error}") from error
    if not isinstance(pointer, dict) or set(pointer) != {
        "schema",
        "type",
        "state",
        "runToken",
        "fullGitSha",
        "manifest",
        "manifestSha256",
        "artifactSha256",
    }:
        raise EvidenceError("latest Rust coverage pointer has a non-canonical envelope")
    token = pointer.get("runToken")
    relative = Path(str(pointer.get("manifest", "")))
    manifest_path = (pointer_path.parent / relative).resolve()
    try:
        manifest_path.relative_to(RUST_COVERAGE_PHASE_ROOT.resolve())
    except ValueError as error:
        raise EvidenceError("latest Rust coverage pointer escapes its immutable root") from error
    if (
        pointer.get("schema") != 1
        or pointer.get("type") != "RustCoveragePrerequisitePointerV1"
        or pointer.get("state") != "COMPLETED"
        or pointer.get("fullGitSha") != expected_sha
        or not isinstance(token, str)
        or manifest_path.parent.name != token
        or manifest_path.name != RUST_COVERAGE_MANIFEST_NAME
        or not manifest_path.is_file()
        or sha256_file(manifest_path) != pointer.get("manifestSha256")
    ):
        raise EvidenceError("latest Rust coverage pointer is incomplete, stale, or substituted")
    manifest = _validate_rust_coverage_phase(
        manifest_path,
        expected_sha=expected_sha,
        expected_checkout_digest=expected_checkout_digest,
        require_fresh=True,
        require_current_environment=True,
    )
    if pointer.get("artifactSha256") != manifest["coverage"]["artifactSha256"]:
        raise EvidenceError("latest Rust coverage pointer names another LLVM artifact")
    return manifest


def rust_coverage_prerequisite_main() -> int:
    """Run the slow coverage campaign as one no-retry, self-supervised prerequisite phase."""

    full_sha = _full_git_sha()
    tree_digest = _source_tree_digest()
    checkout_digest = _checkout_state_digest()
    run_token = uuid.uuid4().hex
    phase_dir = RUST_COVERAGE_PHASE_ROOT / run_token
    phase_dir.mkdir(parents=True, exist_ok=False)
    journal = EvidenceJournal(phase_dir / "events.jsonl", run_token)
    lease = LeaseManager(
        RUST_COVERAGE_LOCK,
        full_sha,
        "rust-coverage-prerequisite",
        run_token,
    )
    phase_ended = False
    try:
        with acquired_lease(lease) as abandoned_token:
            # Starting a new measurement makes an older pointer non-authoritative immediately. A
            # crash/failure can never fall back to the prior success and masquerade as this attempt.
            atomic_write_json(
                RUST_COVERAGE_LATEST,
                {
                    "schema": 1,
                    "type": "RustCoveragePrerequisitePointerV1",
                    "state": "RUNNING",
                    "runToken": run_token,
                    "fullGitSha": full_sha,
                },
            )
            registry = _rust_coverage_command_registry()
            environment = _rust_coverage_environment_document()
            start_record = journal.append(
                "phase_start",
                fullGitSha=full_sha,
                sourceTreeDigest=tree_digest,
                checkoutStateDigest=checkout_digest,
                commandRegistryHash=registry["registrySha256"],
            )
            if abandoned_token is not None:
                journal.append(
                    "abandonment",
                    abandonedRunToken=abandoned_token,
                    reason="stale lease takeover",
                )
                journal.append("phase_end", exitCode=2, verdict="INCOMPLETE_STALE_TAKEOVER")
                phase_ended = True
                print("RUST COVERAGE PREREQUISITE INCOMPLETE: stale-lock takeover; rerun cleanly")
                return 2
            artifact_path = phase_dir / RUST_COVERAGE_ARTIFACT_NAME
            command = [
                sys.executable,
                str(APP / "scripts" / "rust_quality_gate.py"),
                "coverage",
                "--output",
                str(artifact_path),
                "--timeout-seconds",
                str(RUST_COVERAGE_INNER_TIMEOUT_SECONDS),
            ]
            worker_log = phase_dir / "worker.log"
            last_heartbeat = 0.0
            with worker_log.open("x", encoding="utf-8", errors="replace", buffering=1) as log:
                log.write(f"argv={json.dumps(command, ensure_ascii=False)}\ncwd={APP}\n\n")
                log.flush()
                os.fsync(log.fileno())
                process, job = spawn_isolated(
                    command,
                    cwd=APP,
                    log=log,
                    env=_rust_coverage_environment(),
                )
                lease.update_gate("rust-coverage-prerequisite", process.pid)

                def heartbeat() -> None:
                    nonlocal last_heartbeat
                    lease.heartbeat()
                    now = time.monotonic()
                    if now - last_heartbeat >= 5.0:
                        journal.append("heartbeat", childPid=process.pid)
                        last_heartbeat = now

                return_code, timed_out = wait_isolated(
                    process,
                    job,
                    timeout=RUST_COVERAGE_SUPERVISOR_TIMEOUT_SECONDS,
                    heartbeat=heartbeat,
                )
                log.write(f"\nexit={return_code} timedOut={timed_out}\n")
                log.flush()
                os.fsync(log.fileno())
            lease.update_gate(None, None)
            if timed_out or return_code != 0:
                end = journal.append(
                    "phase_end",
                    exitCode=1,
                    verdict="FAIL",
                    timedOut=timed_out,
                    childExitCode=return_code,
                )
                phase_ended = True
                print(f"RUST COVERAGE PREREQUISITE FAILED @ {end['at']}: exit={return_code}")
                return 1
            _assert_source_state(full_sha, tree_digest, checkout_digest)
            coverage = _rust_coverage_report(
                artifact_path,
                coverage_toolchain=environment["coverageToolchain"],
            )
            end_record = journal.append(
                "phase_end",
                exitCode=0,
                verdict="PASS",
                artifactSha256=coverage["artifactSha256"],
            )
            phase_ended = True
            ended = _parse_utc(end_record["at"], "coverage phase end")
            manifest_path = phase_dir / RUST_COVERAGE_MANIFEST_NAME
            manifest = {
                "schema": 1,
                "type": "RustCoveragePrerequisiteV1",
                "complete": True,
                "runToken": run_token,
                "fullGitSha": full_sha,
                "sourceTreeDigest": tree_digest,
                "checkoutStateDigest": checkout_digest,
                "startedAt": start_record["at"],
                "endedAt": end_record["at"],
                "expiresAt": _format_utc(
                    ended + timedelta(seconds=RUST_COVERAGE_FRESH_SECONDS)
                ),
                "exitCode": 0,
                "attemptCount": 1,
                "commandRegistry": registry,
                "environment": environment,
                "coverage": coverage,
                "artifacts": _rust_coverage_phase_artifacts(phase_dir),
            }
            atomic_write_json(manifest_path, manifest)
            _validate_rust_coverage_phase(
                manifest_path,
                expected_sha=full_sha,
                expected_checkout_digest=checkout_digest,
                require_fresh=True,
                require_current_environment=True,
            )
            pointer = {
                "schema": 1,
                "type": "RustCoveragePrerequisitePointerV1",
                "state": "COMPLETED",
                "runToken": run_token,
                "fullGitSha": full_sha,
                "manifest": os.path.relpath(manifest_path, RUST_COVERAGE_LATEST.parent),
                "manifestSha256": sha256_file(manifest_path),
                "artifactSha256": coverage["artifactSha256"],
            }
            publish_validated_json(
                RUST_COVERAGE_LATEST,
                pointer,
                lambda candidate: _validate_latest_rust_coverage_pointer(
                    candidate,
                    expected_sha=full_sha,
                    expected_checkout_digest=checkout_digest,
                ),
            )
            print(f"RUST COVERAGE PREREQUISITE PASS: {manifest_path}")
            return 0
    except KeyboardInterrupt:
        with contextlib.suppress(EvidenceError):
            if not phase_ended:
                journal.append("phase_end", exitCode=130, verdict="ABORTED")
        print("RUST COVERAGE PREREQUISITE ABORTED")
        return 130
    except (EvidenceError, LeaseError, OSError, ValueError, subprocess.SubprocessError) as error:
        with contextlib.suppress(EvidenceError):
            if not phase_ended:
                journal.append("phase_end", exitCode=1, verdict="VERIFIER_FAILURE", detail=str(error))
        print(f"RUST COVERAGE PREREQUISITE VERIFIER FAILURE: {error}")
        return 1


def _consume_rust_coverage_prerequisite(
    run_dir: Path,
    *,
    expected_sha: str,
    expected_checkout_digest: str,
) -> dict[str, object]:
    phase = _validate_latest_rust_coverage_pointer(
        RUST_COVERAGE_LATEST,
        expected_sha=expected_sha,
        expected_checkout_digest=expected_checkout_digest,
    )
    token = str(phase["runToken"])
    source_manifest = RUST_COVERAGE_PHASE_ROOT / token / RUST_COVERAGE_MANIFEST_NAME
    destination_dir = run_dir / "prerequisites" / RUST_COVERAGE_PHASE_DIRNAME / token
    destination_dir.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source_manifest.parent, destination_dir, symlinks=True)
    destination_manifest = destination_dir / RUST_COVERAGE_MANIFEST_NAME
    embedded = _validate_rust_coverage_phase(
        destination_manifest,
        expected_sha=expected_sha,
        expected_checkout_digest=expected_checkout_digest,
        require_fresh=True,
        require_current_environment=True,
    )
    return {
        "path": str(destination_manifest.relative_to(run_dir)),
        "sha256": sha256_file(destination_manifest),
        "bytes": destination_manifest.stat().st_size,
        "runToken": token,
        "fullGitSha": expected_sha,
        "artifactSha256": embedded["coverage"]["artifactSha256"],
        "completedAt": embedded["endedAt"],
        "expiresAt": embedded["expiresAt"],
        "commandRegistryHash": embedded["commandRegistry"]["registrySha256"],
    }


def _validate_rust_coverage_binding(
    value: object,
    *,
    proof_root: Path,
    expected_sha: str,
    expected_checkout_digest: str,
    require_fresh: bool,
) -> dict[str, object]:
    expected_fields = {
        "path",
        "sha256",
        "bytes",
        "runToken",
        "fullGitSha",
        "artifactSha256",
        "completedAt",
        "expiresAt",
        "commandRegistryHash",
    }
    if not isinstance(value, dict) or set(value) != expected_fields:
        raise EvidenceError("product proof has no canonical Rust coverage prerequisite binding")
    token = value.get("runToken")
    expected_relative = str(
        Path("prerequisites")
        / RUST_COVERAGE_PHASE_DIRNAME
        / str(token)
        / RUST_COVERAGE_MANIFEST_NAME
    )
    path = (proof_root / Path(str(value.get("path", "")))).resolve()
    try:
        path.relative_to(proof_root.resolve())
    except ValueError as error:
        raise EvidenceError("Rust coverage prerequisite binding escapes the product proof") from error
    size = value.get("bytes")
    if (
        not isinstance(token, str)
        or not re.fullmatch(r"[0-9a-f]{32}", token)
        or value.get("path") != expected_relative
        or value.get("fullGitSha") != expected_sha
        or not isinstance(size, int)
        or isinstance(size, bool)
        or size <= 0
        or not path.is_file()
        or path.stat().st_size != size
        or sha256_file(path) != value.get("sha256")
    ):
        raise EvidenceError("Rust coverage prerequisite binding is missing, unsafe, or substituted")
    phase = _validate_rust_coverage_phase(
        path,
        expected_sha=expected_sha,
        expected_checkout_digest=expected_checkout_digest,
        require_fresh=require_fresh,
        require_current_environment=False,
    )
    expected_projection = {
        "runToken": phase["runToken"],
        "fullGitSha": phase["fullGitSha"],
        "artifactSha256": phase["coverage"]["artifactSha256"],
        "completedAt": phase["endedAt"],
        "expiresAt": phase["expiresAt"],
        "commandRegistryHash": phase["commandRegistry"]["registrySha256"],
    }
    for field, expected in expected_projection.items():
        if value.get(field) != expected:
            raise EvidenceError(f"Rust coverage prerequisite binding substituted {field}")
    return phase


def _gate_by_id(gate_id: str) -> GateSpec:
    matches = [gate for gate in GATES if gate.id == gate_id]
    if len(matches) != 1:
        raise ValueError(f"unknown or duplicate gate id {gate_id!r}")
    return matches[0]


def gate_worker_main(
    gate_id: str,
    result_path: Path,
    run_token: str,
    authority_mode: str,
    run_authority_digest: str,
) -> int:
    """Execute a probe and gate body in an isolated worker, then atomically publish its result."""

    global LOG_DIR
    gate = _gate_by_id(gate_id)
    effective_environment = _gate_environment(gate, authority_mode)
    environment_authority = _gate_environment_authority(
        gate,
        effective_environment,
        authority_mode=authority_mode,
        run_authority_digest=run_authority_digest,
    )
    LOG_DIR = result_path.parent
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    started = utc_now()
    status, seconds, detail = run_gate(
        gate.id,
        gate.kind,
        gate.payload,
        gate.cwd,
        gate.environment_probe,
        timeout=gate.timeout_seconds,
        authority_mode=authority_mode,
    )
    artifacts = []
    artifact_paths = {
        candidate
        for candidate in LOG_DIR.glob("*.log")
        if candidate.name != "worker.log"
    }
    missing_required: list[str] = []
    for relative_value in gate.artifact_requirements:
        if relative_value in {"attempt-log", "worker-result"}:
            continue
        relative = Path(relative_value)
        candidate = (LOG_DIR / relative).resolve()
        try:
            candidate.relative_to(LOG_DIR.resolve())
        except ValueError:
            missing_required.append(relative_value)
            continue
        if not candidate.is_file():
            missing_required.append(relative_value)
        else:
            artifact_paths.add(candidate)
    if missing_required:
        status = FAIL
        requirement_detail = "required gate artifacts missing: " + ", ".join(missing_required)
        detail = f"{detail}; {requirement_detail}" if detail else requirement_detail
    for path in sorted(artifact_paths):
        artifacts.append(
            {
                "path": str(path.relative_to(LOG_DIR)),
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
            }
        )
    result = {
        "schema": 1,
        "runToken": run_token,
        "gateId": gate.id,
        "startedAt": started,
        "endedAt": utc_now(),
        "status": status,
        "seconds": round(seconds, 3),
        "detail": str(detail),
        "artifacts": artifacts,
        "environmentAuthority": environment_authority,
    }
    atomic_write_json(result_path, result)
    return 0


def _validate_worker_result(
    path: Path,
    gate: GateSpec,
    run_token: str,
    *,
    authority_mode: str,
    run_authority_digest: str,
    expected_environment_authority: dict[str, object],
) -> tuple[str, float, str, list[dict[str, object]], dict[str, object]]:
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"gate {gate.id} has no readable worker result: {error}") from error
    if not isinstance(result, dict) or result.get("schema") != 1:
        raise EvidenceError(f"gate {gate.id} worker result has the wrong schema")
    if result.get("runToken") != run_token or result.get("gateId") != gate.id:
        raise EvidenceError(f"gate {gate.id} worker result is bound to another run/gate")
    environment_authority = _validate_gate_environment_authority(
        result.get("environmentAuthority"),
        gate,
        authority_mode=authority_mode,
        run_authority_digest=run_authority_digest,
    )
    if environment_authority != expected_environment_authority:
        raise EvidenceError(f"gate {gate.id} worker effective environment differs from its supervisor")
    status = result.get("status")
    allowed = {PASS, PASS_AFTER_RETRY, FAIL, SKIP_ENV, NOT_BUILT}
    if status not in allowed:
        raise EvidenceError(f"gate {gate.id} worker returned unknown status {status!r}")
    seconds = result.get("seconds")
    detail = result.get("detail")
    artifacts = result.get("artifacts")
    if not isinstance(seconds, (int, float)) or seconds < 0 or not isinstance(detail, str):
        raise EvidenceError(f"gate {gate.id} worker result has invalid timing/detail")
    if not isinstance(artifacts, list):
        raise EvidenceError(f"gate {gate.id} worker result has no artifact list")
    checked: list[dict[str, object]] = []
    for artifact in artifacts:
        if not isinstance(artifact, dict) or not isinstance(artifact.get("path"), str):
            raise EvidenceError(f"gate {gate.id} worker artifact is malformed")
        artifact_path = path.parent / str(artifact["path"])
        if not artifact_path.is_file() or sha256_file(artifact_path) != artifact.get("sha256"):
            raise EvidenceError(f"gate {gate.id} worker artifact hash mismatch: {artifact_path}")
        checked.append(artifact)
    required_outputs = set(gate.artifact_requirements) - {"attempt-log", "worker-result"}
    observed_outputs = {str(artifact["path"]) for artifact in checked}
    if status in {PASS, PASS_AFTER_RETRY} and not required_outputs <= observed_outputs:
        raise EvidenceError(
            f"gate {gate.id} passed without required artifacts: "
            + ", ".join(sorted(required_outputs - observed_outputs))
        )
    return str(status), float(seconds), detail, checked, environment_authority


def _run_gate_worker(
    gate: GateSpec,
    run_dir: Path,
    run_token: str,
    lease: LeaseManager,
    journal: EvidenceJournal,
    *,
    authority_mode: str,
    run_authority_digest: str,
) -> tuple[str, float, str, list[dict[str, object]], dict[str, object]]:
    gate_dir = run_dir / "gates" / gate.id
    gate_dir.mkdir(parents=True, exist_ok=False)
    result_path = gate_dir / "worker-result.json"
    worker_log = gate_dir / "worker.log"
    worker_environment = _gate_environment(gate, authority_mode)
    environment_authority = _gate_environment_authority(
        gate,
        worker_environment,
        authority_mode=authority_mode,
        run_authority_digest=run_authority_digest,
    )
    environment_digest = str(environment_authority["environmentDigest"])
    journal.append(
        "gate_start",
        gate=gate.id,
        timeoutSeconds=gate.timeout_seconds,
        profiles=sorted(gate.profiles),
        environmentDigest=environment_digest,
    )
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--gate-worker",
        gate.id,
        "--worker-result",
        str(result_path),
        "--run-token",
        run_token,
        "--authority-mode",
        authority_mode,
        "--run-authority-digest",
        run_authority_digest,
    ]
    process = None
    job = None
    last_journal_heartbeat = 0.0
    try:
        with worker_log.open("x", encoding="utf-8", errors="replace", buffering=1) as log:
            process, job = spawn_isolated(
                command,
                cwd=REPO_ROOT,
                log=log,
                env=worker_environment,
            )
            lease.update_gate(gate.id, process.pid)

            def heartbeat() -> None:
                nonlocal last_journal_heartbeat
                lease.heartbeat()
                now = time.monotonic()
                if now - last_journal_heartbeat >= 5.0:
                    journal.append("heartbeat", gate=gate.id, childPid=process.pid)
                    last_journal_heartbeat = now

            return_code, timed_out = wait_isolated(
                process,
                job,
                timeout=gate.timeout_seconds + 45,
                heartbeat=heartbeat,
            )
            log.flush()
            os.fsync(log.fileno())
        lease.update_gate(None, None)
    except KeyboardInterrupt:
        if process is not None and job is not None:
            terminate_isolated(process, job)
        lease.update_gate(None, None)
        journal.append(
            "gate_end",
            gate=gate.id,
            status="ABORTED",
            reason="KeyboardInterrupt",
            environmentDigest=environment_digest,
        )
        raise
    except (OSError, EvidenceError) as error:
        if process is not None and job is not None:
            terminate_isolated(process, job)
        lease.update_gate(None, None)
        detail = f"worker supervision failed: {error}"
        journal.append(
            "gate_end",
            gate=gate.id,
            status=FAIL,
            detail=detail,
            environmentDigest=environment_digest,
        )
        return FAIL, 0.0, detail, [], environment_authority

    worker_artifact = {
        "path": str(worker_log.relative_to(run_dir)),
        "sha256": sha256_file(worker_log),
        "bytes": worker_log.stat().st_size,
    }
    if timed_out:
        detail = f"worker exceeded hard timeout {gate.timeout_seconds + 45}s"
        journal.append(
            "gate_end",
            gate=gate.id,
            status=FAIL,
            detail=detail,
            environmentDigest=environment_digest,
        )
        return (
            FAIL,
            float(gate.timeout_seconds + 45),
            detail,
            [worker_artifact],
            environment_authority,
        )
    if return_code != 0:
        detail = f"worker exited {return_code} without a trustworthy verdict"
        journal.append(
            "gate_end",
            gate=gate.id,
            status=FAIL,
            detail=detail,
            environmentDigest=environment_digest,
        )
        return FAIL, 0.0, detail, [worker_artifact], environment_authority
    try:
        status, seconds, detail, artifacts, environment_authority = _validate_worker_result(
            result_path,
            gate,
            run_token,
            authority_mode=authority_mode,
            run_authority_digest=run_authority_digest,
            expected_environment_authority=environment_authority,
        )
    except EvidenceError as error:
        status, seconds, detail, artifacts = FAIL, 0.0, str(error), []
    normalized = [worker_artifact]
    normalized.extend(
        {
            **artifact,
            "path": str((gate_dir / str(artifact["path"])).relative_to(run_dir)),
        }
        for artifact in artifacts
    )
    normalized.append(
        {
            "path": str(result_path.relative_to(run_dir)),
            "sha256": sha256_file(result_path),
            "bytes": result_path.stat().st_size,
        }
    )
    journal.append(
        "gate_end",
        gate=gate.id,
        status=status,
        seconds=round(seconds, 3),
        detail=detail,
        environmentDigest=environment_digest,
    )
    return status, seconds, detail, normalized, environment_authority


def _manifest_artifacts(run_dir: Path) -> list[dict[str, object]]:
    artifacts = []
    for path in sorted(candidate for candidate in run_dir.rglob("*") if candidate.is_file()):
        if path in {run_dir / "manifest.json", run_dir / PRODUCT_ATTESTATION_NAME}:
            continue
        artifacts.append(
            {
                "path": str(path.relative_to(run_dir)),
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
            }
        )
    return artifacts


EVIDENCE_VALIDATOR_GATES: dict[str, tuple[str, tuple[str, ...], str]] = {
    "architecture-contract": (
        "architecture-contract-evidence",
        (
            "clean-source-tree",
            "architecture-contract-evidence",
            "rust-architecture-truth",
            "python-policies",
            "typecheck",
            "lint-js",
        ),
        _ARCHITECTURE_ARTIFACT,
    ),
    "known-defect-ledger": (
        "known-defect-ledger-evidence",
        ("clean-source-tree", "known-defect-ledger-evidence"),
        _KNOWN_DEFECT_ARTIFACT,
    ),
}


def _validate_class_evidence_artifact(
    class_id: str,
    path: Path,
    *,
    expected_sha: str,
) -> dict[str, object]:
    value = _load_json_without_duplicate_keys(path)
    if (
        not isinstance(value, dict)
        or not isinstance(value.get("schema"), int)
        or isinstance(value.get("schema"), bool)
        or value.get("schema") != 1
    ):
        raise EvidenceError(f"{class_id} evidence artifact has the wrong schema")
    if (
        value.get("classId") != class_id
        or value.get("fullGitSha") != expected_sha
        or value.get("immutableAuthority") != "exact-git-commit"
        or value.get("passed") is not True
        or value.get("failures") != []
    ):
        raise EvidenceError(f"{class_id} evidence artifact is failed, stale, or substituted")
    _parse_utc(value.get("measuredAt"), f"{class_id}.measuredAt")
    if class_id == "known-defect-ledger":
        if value.get("blockingDefectIds") != []:
            raise EvidenceError("known-defect evidence carries unresolved supported-flow blockers")
        if not isinstance(value.get("ledger"), dict) or not isinstance(value.get("defects"), list):
            raise EvidenceError("known-defect evidence omits its ledger or normalized defect rows")
    elif class_id == "architecture-contract":
        rust = value.get("rust")
        ipc = value.get("ipc")
        frontend = value.get("frontend")
        if not isinstance(rust, dict) or rust.get("passed") is not True:
            raise EvidenceError("architecture evidence does not pass the Rust module contract")
        if (
            not isinstance(ipc, dict)
            or ipc.get("handwrittenCount") != 0
            or ipc.get("dynamicCount") != 0
            or ipc.get("noncanonicalErrorCount") != 0
            or not isinstance(ipc.get("generatedCount"), int)
            or isinstance(ipc.get("generatedCount"), bool)
            or ipc.get("generatedCount", 0) <= 0
        ):
            raise EvidenceError("architecture evidence does not prove generated-only static IPC")
        measurements = frontend.get("measurements") if isinstance(frontend, dict) else None
        if (
            not isinstance(measurements, list)
            or not measurements
            or any(
                not isinstance(item, dict)
                or item.get("passed") is not True
                or item.get("directDesktopRuntime") is not False
                for item in measurements
            )
        ):
            raise EvidenceError("architecture evidence does not pass every frontend boundary")
    else:
        raise EvidenceError(f"no semantic validator exists for evidence class {class_id}")
    return value


def _derive_evidence_results(
    profile: str,
    results: list[dict[str, object]],
    proof_root: Path,
    expected_sha: str,
) -> list[dict[str, object]]:
    by_gate: dict[str, dict[str, object]] = {}
    for result in results:
        gate_id = result.get("gateId") if isinstance(result, dict) else None
        if not isinstance(gate_id, str) or gate_id in by_gate:
            raise EvidenceError("cannot derive evidence from missing or duplicate gate results")
        by_gate[gate_id] = result

    derived: list[dict[str, object]] = []
    for spec in _required_evidence_specs(profile):
        binding = EVIDENCE_VALIDATOR_GATES.get(spec.id)
        if binding is None:
            derived.append(
                {
                    "classId": spec.id,
                    "status": PENDING_EXTERNAL,
                    "detail": spec.description,
                }
            )
            continue
        validator_gate, prerequisites, artifact_name = binding
        missing = [gate_id for gate_id in prerequisites if gate_id not in by_gate]
        if missing:
            derived.append(
                {
                    "classId": spec.id,
                    "status": EVIDENCE_FAILED,
                    "detail": (
                        "class-specific validator prerequisites are absent: "
                        + ", ".join(missing)
                    ),
                }
            )
            continue
        non_pass = [
            gate_id for gate_id in prerequisites if by_gate[gate_id].get("status") != PASS
        ]
        if non_pass:
            derived.append(
                {
                    "classId": spec.id,
                    "status": EVIDENCE_FAILED,
                    "detail": (
                        "class-specific validation did not pass: " + ", ".join(non_pass)
                    ),
                }
            )
            continue
        expected_relative = f"gates/{validator_gate}/{artifact_name}"
        artifacts = by_gate[validator_gate].get("artifacts")
        if not isinstance(artifacts, list):
            raise EvidenceError(f"{spec.id} validator gate has no artifact inventory")
        matches = [
            artifact
            for artifact in artifacts
            if isinstance(artifact, dict) and artifact.get("path") == expected_relative
        ]
        if len(matches) != 1:
            raise EvidenceError(f"{spec.id} validator has no unique required artifact")
        artifact = matches[0]
        artifact_path = (proof_root / expected_relative).resolve()
        try:
            artifact_path.relative_to(proof_root.resolve())
        except ValueError as error:
            raise EvidenceError(f"{spec.id} evidence artifact escapes the proof root") from error
        size = artifact.get("bytes")
        if (
            not artifact_path.is_file()
            or not isinstance(size, int)
            or isinstance(size, bool)
            or size <= 0
            or artifact_path.stat().st_size != size
            or sha256_file(artifact_path) != artifact.get("sha256")
        ):
            raise EvidenceError(f"{spec.id} evidence artifact hash/size binding is invalid")
        document = _validate_class_evidence_artifact(
            spec.id,
            artifact_path,
            expected_sha=expected_sha,
        )
        derived.append(
            {
                "classId": spec.id,
                "status": EVIDENCE_VERIFIED,
                "detail": spec.description,
                "measuredAt": document["measuredAt"],
                "immutableAuthority": {
                    "kind": "exact-git-commit",
                    "fullGitSha": expected_sha,
                },
                "evidence": {
                    "gateId": validator_gate,
                    "path": expected_relative,
                    "sha256": artifact["sha256"],
                    "bytes": size,
                },
            }
        )
    return derived


def _validate_evidence_results(
    profile: str,
    value: object,
    *,
    results: list[dict[str, object]] | None = None,
    proof_root: Path | None = None,
    expected_sha: str | None = None,
) -> list[dict[str, object]]:
    if results is None or proof_root is None or expected_sha is None:
        expected: list[dict[str, object]] = list(_pending_evidence_results(profile))
    else:
        expected = _derive_evidence_results(profile, results, proof_root, expected_sha)
    if value != expected:
        raise EvidenceError(
            "certification evidence classes were omitted, substituted, reordered, or self-asserted; "
            "implemented results must be rederived from exact validator artifacts and unimplemented "
            "classes must remain pending"
        )
    return expected


def _validate_schema_authority(
    value: object, expected_sha: str, proof_root: Path, *, eligible: bool
) -> None:
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise EvidenceError("product proof has no schema-1 migration authority")
    relative = MIGRATION_CATALOG.relative_to(REPO_ROOT).as_posix()
    committed = _git_file_bytes(expected_sha, relative)
    committed_history = _migration_history(committed)
    committed_history_bytes = json.dumps(
        committed_history, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")
    committed_fields = {
        "catalogPath": relative,
        "catalogProofPath": "source-authority/migrations.rs",
        "catalogGitBlobSha1": _git_blob_id(expected_sha, relative),
        "committedCatalogSha256": hashlib.sha256(committed).hexdigest(),
        "committedLatestVersion": committed_history[-1][0],
        "committedMigrationCount": len(committed_history),
        "committedHistorySha256": hashlib.sha256(committed_history_bytes).hexdigest(),
    }
    for field, expected in committed_fields.items():
        if value.get(field) != expected:
            raise EvidenceError(f"product proof substituted schema authority field {field}")
    for field in ("catalogSha256", "historySha256"):
        if not isinstance(value.get(field), str) or not re.fullmatch(r"[0-9a-f]{64}", str(value[field])):
            raise EvidenceError(f"product proof schema authority has invalid {field}")
    for field in ("latestVersion", "migrationCount"):
        if not isinstance(value.get(field), int) or isinstance(value.get(field), bool) or value[field] <= 0:
            raise EvidenceError(f"product proof schema authority has invalid {field}")
    proof_relative = Path(str(value["catalogProofPath"]))
    proof_path = (proof_root / proof_relative).resolve()
    try:
        proof_path.relative_to(proof_root.resolve())
    except ValueError as error:
        raise EvidenceError("schema authority proof escapes the immutable run") from error
    if not proof_path.is_file():
        raise EvidenceError("schema authority catalog proof is missing")
    proof_bytes = proof_path.read_bytes()
    proof_history = _migration_history(proof_bytes)
    proof_history_bytes = json.dumps(
        proof_history, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")
    if (
        hashlib.sha256(proof_bytes).hexdigest() != value.get("catalogSha256")
        or proof_history[-1][0] != value.get("latestVersion")
        or len(proof_history) != value.get("migrationCount")
        or hashlib.sha256(proof_history_bytes).hexdigest() != value.get("historySha256")
    ):
        raise EvidenceError("schema authority differs from its hash-listed catalog proof")

    matches = value.get("matchesCommittedSource")
    if not isinstance(matches, bool):
        raise EvidenceError("product proof schema authority has no source-match verdict")
    derived_match = (
        proof_bytes == committed
        and value.get("catalogSha256") == committed_fields["committedCatalogSha256"]
        and value.get("historySha256") == committed_fields["committedHistorySha256"]
        and value.get("latestVersion") == committed_fields["committedLatestVersion"]
        and value.get("migrationCount") == committed_fields["committedMigrationCount"]
    )
    if matches != derived_match:
        raise EvidenceError("product proof schema source-match verdict is not derivable")
    if eligible and not matches:
        raise EvidenceError("a certifying proof is not bound to the committed migration catalog")


def _validate_release_artifacts(
    profile: str, value: object, expected_sha: str, *, eligible: bool
) -> list[dict[str, object]]:
    if not isinstance(value, list):
        raise EvidenceError("product proof has no release-artifact bindings")
    roles: list[str] = []
    for artifact in value:
        if not isinstance(artifact, dict):
            raise EvidenceError("product proof contains a malformed release artifact")
        role = artifact.get("role")
        digest = artifact.get("sha256")
        size = artifact.get("bytes")
        name = artifact.get("name")
        if (
            not isinstance(role, str)
            or not role
            or not isinstance(name, str)
            or not name
            or not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
            or not isinstance(size, int)
            or isinstance(size, bool)
            or size <= 0
        ):
            raise EvidenceError("product proof contains an invalid release-artifact identity")
        roles.append(role)
    if len(roles) != len(set(roles)):
        raise EvidenceError("product proof contains duplicate release-artifact roles")
    missing = sorted(set(PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[profile]) - set(roles))
    if eligible and missing:
        raise EvidenceError(f"certifying proof omits release-artifact roles: {', '.join(missing)}")
    if eligible and profile in {PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_FULL}:
        executable = next(
            (artifact for artifact in value if artifact.get("role") == "application-executable"),
            None,
        )
        if (
            executable is None
            or executable.get("matchesFullGitSha") is not True
            or executable.get("buildGitSha") != expected_sha
            or executable.get("authority") != "active-immutable-release"
            or executable.get("activeReleaseGitSha") != expected_sha
        ):
            raise EvidenceError("certifying product proof lacks the exact immutable release executable")
    if eligible and profile in {PROFILE_WINDOWS, PROFILE_FULL}:
        by_role = {str(artifact["role"]): artifact for artifact in value}
        for role in ("application-executable", "windows-msi", "windows-nsis"):
            authenticode = by_role[role].get("authenticode")
            if (
                not isinstance(authenticode, dict)
                or authenticode.get("status") != "Valid"
                or authenticode.get("timestampVerified") is not True
                or not re.fullmatch(
                    r"[0-9A-F]{40}", str(authenticode.get("signerThumbprintSha1", ""))
                )
                or not re.fullmatch(
                    r"[0-9A-F]{64}", str(authenticode.get("signerCertificateSha256", ""))
                )
            ):
                raise EvidenceError(f"certifying Windows proof lacks verified Authenticode for {role}")
        provenance = by_role["github-sigstore-provenance"]
        if (
            provenance.get("provenanceVerified") is not True
            or provenance.get("sourceGitSha") != expected_sha
            or not isinstance(provenance.get("signerWorkflow"), str)
        ):
            raise EvidenceError("certifying Windows proof lacks exact GitHub/Sigstore provenance")
        if by_role["release-checksums"].get("inventoryVerified") is not True:
            raise EvidenceError("certifying Windows proof lacks an exact checksum inventory")
        if by_role["cyclonedx-sbom"].get("sbomVerified") is not True:
            raise EvidenceError("certifying Windows proof lacks a validated CycloneDX SBOM")
        updater_signature = by_role["windows-updater"].get("signature")
        if (
            not isinstance(updater_signature, dict)
            or updater_signature.get("verified") is not True
            or not re.fullmatch(r"[0-9a-f]{64}", str(updater_signature.get("sha256", "")))
            or not re.fullmatch(
                r"[0-9a-f]{64}", str(updater_signature.get("publicKeySha256", ""))
            )
            or not re.fullmatch(
                r"[0-9a-f]{64}", str(updater_signature.get("verifierSourceSha256", ""))
            )
            or not isinstance(updater_signature.get("bytes"), int)
            or isinstance(updater_signature.get("bytes"), bool)
            or updater_signature.get("bytes", 0) <= 0
        ):
            raise EvidenceError(
                "certifying Windows proof lacks a cryptographically verified updater signature"
            )
    return value


def _bind_proof_to_windows_release_bundle(
    manifest: dict[str, object], authority: dict[str, object]
) -> None:
    """Require every measured bundle identity to be carried verbatim by the product proof."""

    source = authority.get("source")
    if not isinstance(source, dict) or source.get("fullGitSha") != manifest.get("fullGitSha"):
        raise EvidenceError("Windows release bundle and certifying proof name different source commits")
    measured = _authority_release_artifacts(authority)
    recorded_value = manifest.get("releaseArtifacts")
    if not isinstance(recorded_value, list):
        raise EvidenceError("certifying proof has no release-artifact inventory")
    recorded = {
        str(artifact.get("role")): artifact
        for artifact in recorded_value
        if isinstance(artifact, dict) and isinstance(artifact.get("role"), str)
    }
    measured_roles = {str(artifact.get("role")) for artifact in measured}
    required_roles = set(PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[str(manifest["profile"])])
    if not required_roles <= measured_roles:
        raise EvidenceError(
            "re-observed Windows bundle omits roles required by the certifying profile: "
            + ", ".join(sorted(required_roles - measured_roles))
        )
    for artifact in measured:
        role = str(artifact["role"])
        proof_artifact = recorded.get(role)
        if proof_artifact is None:
            raise EvidenceError(f"certifying proof does not bind the measured {role} artifact")
        # The active executable binding carries additional live deployment fields.  Every field
        # measured from the release bundle must nevertheless be present and byte-for-byte equal.
        for field, value in artifact.items():
            if proof_artifact.get(field) != value:
                raise EvidenceError(
                    f"certifying proof {role} identity differs from the re-observed release bundle"
                )


def _persist_windows_release_authority(run_dir: Path) -> dict[str, object] | None:
    """Persist the complete independently measured Windows bundle report into this proof.

    ``releaseArtifacts`` is the compact product contract used by existing proof consumers.  The
    complete validation report is also needed for a forensic proof bundle: it carries the exact
    source/ref/version and signer pins that produced those projections.  Storing it inside the run
    directory makes those bytes part of the manifest's exact artifact inventory instead of relying
    on process-local state that disappears after verification.
    """

    if _WINDOWS_RELEASE_AUTHORITY is None:
        return None
    # Refuse to persist a caller-created status document.  Only the process-local value populated by
    # ``configure_windows_release_authority`` reaches here, and its crypto flags must still satisfy
    # the verifier's structural contract before publication.
    _authority_release_artifacts(_WINDOWS_RELEASE_AUTHORITY)
    authority_path = run_dir / WINDOWS_RELEASE_AUTHORITY_NAME
    atomic_write_json(authority_path, _WINDOWS_RELEASE_AUTHORITY)
    return {
        "path": WINDOWS_RELEASE_AUTHORITY_NAME,
        "sha256": sha256_file(authority_path),
        "bytes": authority_path.stat().st_size,
    }


def _validate_windows_release_authority_binding(
    value: object,
    *,
    proof_root: Path,
    manifest: dict[str, object],
    expected_sha: str,
    eligible: bool,
    profile: str,
) -> dict[str, object] | None:
    """Validate the proof-local full Windows release report and its artifact projection."""

    required = eligible and profile in {PROFILE_WINDOWS, PROFILE_FULL}
    if value is None:
        if required:
            raise EvidenceError(
                "certifying Windows/full proof omits its complete release-bundle authority"
            )
        return None
    if not isinstance(value, dict) or set(value) != {"path", "sha256", "bytes"}:
        raise EvidenceError("proof has a malformed Windows release-authority binding")
    digest = value.get("sha256")
    size = value.get("bytes")
    if (
        value.get("path") != WINDOWS_RELEASE_AUTHORITY_NAME
        or not isinstance(digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", digest)
        or not isinstance(size, int)
        or isinstance(size, bool)
        or size <= 0
    ):
        raise EvidenceError("proof has an invalid Windows release-authority identity")
    authority_path = proof_root / WINDOWS_RELEASE_AUTHORITY_NAME
    if (
        not authority_path.is_file()
        or authority_path.stat().st_size != size
        or sha256_file(authority_path) != digest
    ):
        raise EvidenceError("proof Windows release-authority bytes are missing or changed")
    try:
        authority = json.loads(authority_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"proof Windows release authority cannot be read: {error}") from error
    if not isinstance(authority, dict) or set(authority) != {
        "schema",
        "type",
        "source",
        "signer",
        "cryptographicValidation",
        "certificationReady",
        "artifacts",
    }:
        raise EvidenceError("proof Windows release authority has a non-canonical envelope")
    source = authority.get("source")
    signer = authority.get("signer")
    ready = authority.get("certificationReady")
    if (
        not isinstance(source, dict)
        or set(source) != {"fullGitSha", "repository", "ref", "version"}
        or source.get("fullGitSha") != expected_sha
        or not all(
            isinstance(source.get(field), str) and bool(source.get(field))
            for field in ("repository", "ref", "version")
        )
        or not isinstance(signer, dict)
        or set(signer) != {"thumbprintSha1", "certificateSha256"}
        or not re.fullmatch(r"[0-9A-F]{40}", str(signer.get("thumbprintSha1", "")))
        or not re.fullmatch(r"[0-9A-F]{64}", str(signer.get("certificateSha256", "")))
        or not isinstance(ready, bool)
    ):
        raise EvidenceError("proof Windows release authority lacks exact source/signer identity")
    _authority_release_artifacts(authority)
    _bind_proof_to_windows_release_bundle(manifest, authority)
    if required and ready is not True:
        raise EvidenceError("certifying Windows/full proof uses a draft-only release bundle")
    return authority


def _revalidate_latest_release_executable(
    profile: str,
    value: list[dict[str, object]],
    expected_sha: str,
) -> None:
    """Re-observe mutable deployment state when consuming ``latest-proof``.

    A completed proof bundle is historical and remains verifiable from its hash chain after the
    workstation changes.  ``latest-proof`` has a stronger meaning: it is the authority for the
    release that is active *now*.  The active-release pointer and executable live outside Git, so a
    checkout-state digest cannot detect their replacement after the proof was published.  Comparing
    the recorded application binding with a fresh observation prevents a structurally valid but
    deployment-stale pointer from continuing to validate.

    Windows installer/SBOM/provenance roles are immutable bundle authorities rather than active
    workstation state.  Their future consumer needs the downloaded bundle directory and independent
    Authenticode/Sigstore validators; this local recheck deliberately makes no claim about them.
    """

    if profile not in {PROFILE_OWNER, PROFILE_WINDOWS, PROFILE_FULL}:
        return
    recorded = next(
        (artifact for artifact in value if artifact.get("role") == "application-executable"),
        None,
    )
    observed = next(
        (
            artifact
            for artifact in _release_artifact_bindings(expected_sha)
            if artifact.get("role") == "application-executable"
        ),
        None,
    )
    live_fields = (
        "role",
        "name",
        "sha256",
        "bytes",
        "buildGitSha",
        "matchesFullGitSha",
        "authority",
        "activeReleasePointerSha256",
        "activeReleaseGitSha",
    )
    if recorded is None or observed is None or any(
        recorded.get(field) != observed.get(field) for field in live_fields
    ):
        raise EvidenceError(
            "latest-proof release executable or active immutable release pointer changed after measurement"
        )


def _validate_model_attestation_binding(value: object, expected_sha: str, *, eligible: bool, profile: str) -> None:
    required = profile in {PROFILE_MODEL, PROFILE_FULL}
    if value is None:
        if eligible and required:
            raise EvidenceError("certifying model proof has no current model-attestation binding")
        return
    if not isinstance(value, dict):
        raise EvidenceError("product proof has a malformed model-attestation binding")
    relative = MODEL_ATTESTATION.relative_to(REPO_ROOT).as_posix()
    digest = value.get("sha256")
    size = value.get("bytes")
    if (
        value.get("path") != relative
        or not isinstance(digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", digest)
        or not isinstance(size, int)
        or isinstance(size, bool)
        or size <= 0
    ):
        raise EvidenceError("product proof has an invalid model-attestation identity")
    try:
        committed = _git_file_bytes(expected_sha, relative)
    except EvidenceError:
        if eligible and required:
            raise
        return
    if hashlib.sha256(committed).hexdigest() != digest or len(committed) != size:
        raise EvidenceError("product proof substituted its model-attestation bytes")


def _product_attestation_document(
    manifest_path: Path, manifest: dict[str, object]
) -> dict[str, object]:
    return {
        "schema": 1,
        "type": "ProductAttestationV1",
        "runToken": manifest["runToken"],
        "profile": manifest["profile"],
        "proofManifest": {
            "path": manifest_path.name,
            "sha256": sha256_file(manifest_path),
        },
        "source": {
            "fullGitSha": manifest["fullGitSha"],
            "sourceTreeDigest": manifest["sourceTreeDigest"],
            "checkoutStateDigest": manifest["checkoutStateDigest"],
        },
        "gateRegistryHash": manifest["gateRegistryHash"],
        "evidenceContractHash": manifest["evidenceContractHash"],
        "releaseEnvironment": manifest["environment"],
        "runAuthority": manifest["runAuthority"],
        "schemaAuthority": manifest["schemaAuthority"],
        "releaseArtifacts": manifest["releaseArtifacts"],
        "windowsReleaseAuthority": manifest.get("windowsReleaseAuthority"),
        "rustCoveragePrerequisite": manifest.get("rustCoveragePrerequisite"),
        "knownDefectDigest": manifest["knownDefectDigest"],
        "modelAttestation": manifest["modelAttestation"],
        "staleTakeover": manifest["staleTakeover"],
        "certificationEligible": manifest["certificationEligible"],
        "verdict": manifest["verdict"],
    }


def _validate_product_attestation(
    attestation_path: Path,
    manifest_path: Path,
    manifest: dict[str, object],
) -> dict[str, object]:
    try:
        attestation = json.loads(attestation_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"product attestation cannot be read: {error}") from error
    expected = _product_attestation_document(manifest_path, manifest)
    if attestation != expected:
        raise EvidenceError("product attestation is missing, stale, or substituted")
    return attestation


def _safe_pointer_target(pointer_path: Path, relative_value: object, label: str) -> Path:
    if not isinstance(relative_value, str):
        raise EvidenceError(f"latest proof has no {label} path")
    relative = Path(relative_value)
    if relative.is_absolute() or str(relative) in {"", "."}:
        raise EvidenceError(f"latest proof has an unsafe {label} path")
    target = (pointer_path.parent / relative).resolve()
    try:
        target.relative_to(PROOF_ROOT.resolve())
    except ValueError as error:
        raise EvidenceError(f"latest proof {label} escapes the immutable proof root") from error
    return target


def _validate_latest_proof(path: Path, expected_sha: str | None = None) -> dict[str, object]:
    try:
        pointer = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"latest-proof pointer cannot be read: {error}") from error
    if not isinstance(pointer, dict) or pointer.get("schema") != 1:
        raise EvidenceError("latest-proof pointer has the wrong schema")
    expected_sha = _full_git_sha() if expected_sha is None else expected_sha
    full_sha = pointer.get("fullGitSha")
    token = pointer.get("runToken")
    if (
        not isinstance(full_sha, str)
        or not re.fullmatch(r"[0-9a-f]{40}", full_sha)
        or not isinstance(token, str)
        or not token
        or full_sha != expected_sha
    ):
        raise EvidenceError("latest-proof pointer has the wrong source/run identity")
    manifest_path = _safe_pointer_target(path, pointer.get("manifest"), "manifest")
    attestation_path = _safe_pointer_target(path, pointer.get("productAttestation"), "product attestation")
    if (
        manifest_path.name != "manifest.json"
        or manifest_path.parent.name != token
        or manifest_path.parent != attestation_path.parent
        or attestation_path.name != PRODUCT_ATTESTATION_NAME
    ):
        raise EvidenceError("latest-proof roots disagree")
    if not manifest_path.is_file() or sha256_file(manifest_path) != pointer.get("manifestSha256"):
        raise EvidenceError("latest-proof manifest hash is missing or changed")
    if not attestation_path.is_file() or sha256_file(attestation_path) != pointer.get(
        "productAttestationSha256"
    ):
        raise EvidenceError("latest-proof product-attestation hash is missing or changed")
    manifest = _validate_completed_manifest(
        manifest_path,
        full_sha,
        token,
        require_current_live_authority=True,
    )
    # ``latest-proof`` is the authority for the checkout somebody is looking at now, not merely for
    # any historical byte state that happened to share HEAD.  A clean proof followed by uncommitted
    # application edits has the same Git SHA, so SHA matching alone would keep stale status alive.
    # Historical/offsite bundles remain independently verifiable through _validate_completed_manifest.
    if _full_git_sha() == full_sha and _checkout_state_digest() != manifest.get("checkoutStateDigest"):
        raise EvidenceError("latest-proof checkout state differs from the current working tree")
    if pointer.get("profile") != manifest.get("profile"):
        raise EvidenceError("latest-proof profile differs from its manifest")
    _revalidate_latest_release_executable(
        str(manifest["profile"]),
        manifest["releaseArtifacts"],
        full_sha,
    )
    _validate_product_attestation(attestation_path, manifest_path, manifest)
    return manifest


def _require_latest_certifying_proof(
    path: Path,
    expected_profile: str,
    expected_sha: str | None = None,
    release_authority: dict[str, object] | None = None,
) -> dict[str, object]:
    """Consume, rather than merely publish, one exact current certifying proof.

    Structural validation alone is intentionally useful for incomplete diagnostic runs.  A release
    decision must additionally demand the requested profile, full (non-quick) execution, and the
    reconstructed certification verdict.  Keeping those checks in one verifier-owned consumer
    prevents callers from treating "well-formed proof" as "certified product".
    """

    if expected_profile not in PROFILES:
        raise EvidenceError(f"unknown proof profile {expected_profile!r}")
    manifest = _validate_latest_proof(path, expected_sha)
    return _require_certifying_manifest(manifest, expected_profile, release_authority)


def _require_certifying_manifest(
    manifest: dict[str, object],
    expected_profile: str,
    release_authority: dict[str, object] | None = None,
) -> dict[str, object]:
    """Apply the non-negotiable release verdict and optional exact-bundle binding."""

    if expected_profile not in PROFILES:
        raise EvidenceError(f"unknown proof profile {expected_profile!r}")
    if manifest.get("profile") != expected_profile:
        raise EvidenceError(
            f"proof profile {manifest.get('profile')!r} does not satisfy {expected_profile!r}"
        )
    if manifest.get("quick") is not False:
        raise EvidenceError("proof used quick mode and cannot authorize a release")
    if (
        manifest.get("certificationEligible") is not True
        or manifest.get("exitCode") != 0
        or manifest.get("requiredEvidencePending") != []
    ):
        raise EvidenceError("proof is complete but not certification-eligible")
    expected_verdict = _profile_verdict(
        expected_profile,
        False,
        [
            (
                str(result["gateId"]),
                str(result["status"]),
                float(result["seconds"]),
                str(result["detail"]),
            )
            for result in manifest["results"]
        ],
        manifest["certificationEvidence"],
        stale_takeover=False,
        diagnostic_authority_overrides=(
            _validate_run_authority(manifest.get("runAuthority"))[0]
            == AUTHORITY_MODE_DIAGNOSTIC
        ),
    )[1]
    if manifest.get("verdict") != expected_verdict:
        raise EvidenceError("proof does not carry the exact certifying profile verdict")
    if expected_profile in {PROFILE_WINDOWS, PROFILE_FULL}:
        if release_authority is None:
            raise EvidenceError(
                "Windows/full product proof consumption requires independent re-observation of the exact release bundle"
            )
        _bind_proof_to_windows_release_bundle(manifest, release_authority)
    return manifest


def _require_detached_certifying_proof(
    manifest_path: Path,
    expected_profile: str,
    expected_sha: str,
    release_authority: dict[str, object] | None = None,
) -> dict[str, object]:
    """Consume an immutable proof directory downloaded by a release workflow.

    Unlike ``latest-proof`` this has no claim about the workstation active *now*, so it does not
    re-read the local active-release pointer.  It still validates the entire manifest, product
    attestation, gate registry, event journal, source Git object, and artifact inventory before the
    certifying verdict is considered.  The supplied bundle is then independently re-measured.
    """

    manifest_path = manifest_path.resolve(strict=True)
    if manifest_path.name != "manifest.json":
        raise EvidenceError("detached proof authority must be named manifest.json")
    try:
        envelope = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"detached proof manifest cannot be read: {error}") from error
    token = envelope.get("runToken") if isinstance(envelope, dict) else None
    if not isinstance(token, str) or not token or manifest_path.parent.name != token:
        raise EvidenceError("detached proof directory does not match its run token")
    manifest = _validate_completed_manifest(manifest_path, expected_sha, token)
    return _require_certifying_manifest(manifest, expected_profile, release_authority)


def _validate_stale_takeover(value: object) -> tuple[bool, str | None]:
    """Validate the recovery marker that permanently makes this run non-certifying.

    Taking over a verified-stale lease is necessary for self-healing, but the final certification
    contract explicitly forbids lock recovery.  The recovery fact therefore belongs in the immutable
    manifest and cannot be inferred only from an easy-to-overlook journal line.
    """

    if not isinstance(value, dict) or set(value) != {"occurred", "abandonedRunToken"}:
        raise EvidenceError("proof manifest has no exact stale-takeover authority")
    occurred = value.get("occurred")
    abandoned_token = value.get("abandonedRunToken")
    if not isinstance(occurred, bool):
        raise EvidenceError("proof manifest has an invalid stale-takeover verdict")
    if occurred:
        if not isinstance(abandoned_token, str) or not abandoned_token:
            raise EvidenceError("stale-takeover proof has no abandoned run token")
        return True, abandoned_token
    if abandoned_token is not None:
        raise EvidenceError("clean proof run unexpectedly names an abandoned run token")
    return False, None


def _validate_completed_manifest(
    path: Path,
    expected_sha: str,
    expected_token: str,
    *,
    require_current_live_authority: bool = False,
) -> dict[str, object]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"completed proof manifest cannot be read: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema") != 1:
        raise EvidenceError("completed proof manifest has the wrong schema")
    if manifest.get("fullGitSha") != expected_sha or manifest.get("runToken") != expected_token:
        raise EvidenceError("completed proof manifest is bound to another source/run")
    if manifest.get("complete") is not True or not isinstance(manifest.get("results"), list):
        raise EvidenceError("proof manifest is not complete")
    profile = manifest.get("profile")
    quick = manifest.get("quick")
    if profile not in PROFILES or not isinstance(quick, bool):
        raise EvidenceError("proof manifest has an invalid profile/mode")
    certification_eligible = manifest.get("certificationEligible")
    if not isinstance(certification_eligible, bool):
        raise EvidenceError("proof manifest has no certification-eligibility verdict")
    expected_tree = _source_tree_digest_for_sha(expected_sha)
    if manifest.get("sourceTreeDigest") != expected_tree:
        raise EvidenceError("proof manifest source tree does not match its Git commit")
    checkout_digest = manifest.get("checkoutStateDigest")
    if not isinstance(checkout_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", checkout_digest):
        raise EvidenceError("proof manifest has no checkout-state digest")
    stale_takeover, abandoned_run_token = _validate_stale_takeover(manifest.get("staleTakeover"))

    registry_path = path.parent / "gate-registry.json"
    environment_path = path.parent / "environment.json"
    run_authority_path = path.parent / RUN_AUTHORITY_NAME
    evidence_contract_path = path.parent / EVIDENCE_CONTRACT_NAME
    try:
        registry = json.loads(registry_path.read_text(encoding="utf-8"))
        stored_environment = json.loads(environment_path.read_text(encoding="utf-8"))
        stored_run_authority = json.loads(run_authority_path.read_text(encoding="utf-8"))
        stored_evidence_contract = json.loads(evidence_contract_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"proof registry/environment cannot be read: {error}") from error
    if registry != gate_registry_document():
        raise EvidenceError("proof gate registry differs from the verifier registry")
    canonical_registry = json.dumps(
        registry, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    if hashlib.sha256(canonical_registry).hexdigest() != manifest.get("gateRegistryHash"):
        raise EvidenceError("proof gate-registry hash is invalid")
    if stored_environment != manifest.get("environment"):
        raise EvidenceError("proof environment document differs from the manifest")
    if stored_run_authority != manifest.get("runAuthority"):
        raise EvidenceError("proof live-authority document differs from the manifest")
    authority_mode, run_authority_digest = _validate_run_authority(stored_run_authority)
    if require_current_live_authority and authority_mode == AUTHORITY_MODE_LIVE:
        expected_live_authority = _run_authority_document(
            diagnostic_overrides=False,
            caller_environment={},
        )
        if (
            stored_run_authority.get("roots") != expected_live_authority.get("roots")
            or stored_run_authority.get("targets") != expected_live_authority.get("targets")
        ):
            raise EvidenceError("proof live authority is not the current Windows Known Folder authority")
    if stored_evidence_contract != evidence_contract_document():
        raise EvidenceError("proof evidence contract differs from the verifier contract")
    if manifest.get("evidenceContractHash") != evidence_contract_hash():
        raise EvidenceError("proof evidence-contract hash is invalid")

    expected_gates = [gate for gate in GATES if profile in gate.profiles]
    expected_ids = [gate.id for gate in expected_gates]
    results = manifest["results"]
    actual_ids: list[str] = []
    normalized_results: list[tuple[str, str, float, str]] = []
    allowed_statuses = {PASS, PASS_AFTER_RETRY, FAIL, SKIP_ENV, NOT_BUILT, NOT_RUN_QUICK}
    result_artifacts: list[dict[str, object]] = []
    result_environment_digests: dict[str, str] = {}
    for result, gate in zip(results, expected_gates, strict=False):
        if not isinstance(result, dict):
            raise EvidenceError("proof manifest contains a malformed gate result")
        gate_id = result.get("gateId")
        status = result.get("status")
        seconds = result.get("seconds")
        detail = result.get("detail")
        if not isinstance(gate_id, str) or status not in allowed_statuses:
            raise EvidenceError("proof manifest contains an invalid gate identity/status")
        if not isinstance(seconds, (int, float)) or isinstance(seconds, bool) or seconds < 0:
            raise EvidenceError(f"proof gate {gate_id} has invalid timing")
        if not isinstance(detail, str):
            raise EvidenceError(f"proof gate {gate_id} has invalid detail")
        if quick and gate.tier > 1:
            if (
                status != NOT_RUN_QUICK
                or result.get("artifacts") not in (None, [])
                or result.get("environmentAuthority") is not None
            ):
                raise EvidenceError(f"quick proof gate {gate_id} is not an honest omission")
        elif status == NOT_RUN_QUICK:
            raise EvidenceError(f"proof gate {gate_id} was omitted outside quick-mode policy")
        else:
            environment_authority = _validate_gate_environment_authority(
                result.get("environmentAuthority"),
                gate,
                authority_mode=authority_mode,
                run_authority_digest=run_authority_digest,
            )
            result_environment_digests[gate.id] = str(
                environment_authority["environmentDigest"]
            )
            if gate.id in LIVE_AUTHORITY_GATE_IDS:
                binding_by_name = {
                    str(item["name"]): item
                    for item in environment_authority["effectiveEnvironment"]
                }
                roots = stored_run_authority["roots"]
                if (
                    binding_by_name.get("APPDATA", {}).get("pathSha256")
                    != roots["roamingAppData"]["absolutePathSha256"]
                    or binding_by_name.get("LOCALAPPDATA", {}).get("pathSha256")
                    != roots["localAppData"]["absolutePathSha256"]
                ):
                    raise EvidenceError(
                        f"live gate {gate.id} did not execute against the run's audited data roots"
                    )
            gate_artifacts = result.get("artifacts")
            if not isinstance(gate_artifacts, list) or not gate_artifacts:
                raise EvidenceError(f"proof gate {gate_id} has no durable worker artifacts")
            result_artifacts.extend(gate_artifacts)
        actual_ids.append(gate_id)
        normalized_results.append((gate_id, str(status), float(seconds), detail))
    if len(results) != len(expected_gates) or actual_ids != expected_ids or len(set(actual_ids)) != len(actual_ids):
        raise EvidenceError("proof results do not exactly match the ordered selected gate set")

    evidence_results = _validate_evidence_results(
        str(profile),
        manifest.get("certificationEvidence"),
        results=results,
        proof_root=path.parent,
        expected_sha=expected_sha,
    )
    reconstructed_code, reconstructed_verdict = _profile_verdict(
        str(profile),
        quick,
        normalized_results,
        evidence_results,
        stale_takeover=stale_takeover,
        diagnostic_authority_overrides=(authority_mode == AUTHORITY_MODE_DIAGNOSTIC),
    )
    if manifest.get("exitCode") != reconstructed_code or manifest.get("verdict") != reconstructed_verdict:
        raise EvidenceError("proof verdict/exit code cannot be reconstructed from gate results")
    pending_ids = [result["classId"] for result in evidence_results if result["status"] != "VERIFIED"]
    if manifest.get("requiredEvidencePending") != pending_ids:
        raise EvidenceError("proof required-evidence inventory is incomplete or altered")
    if certification_eligible != (reconstructed_code == 0):
        raise EvidenceError("proof certification eligibility contradicts its reconstructed verdict")
    if certification_eligible and authority_mode != AUTHORITY_MODE_LIVE:
        raise EvidenceError("proof is marked certifying with diagnostic live authority")
    _validate_schema_authority(
        manifest.get("schemaAuthority"),
        expected_sha,
        path.parent,
        eligible=certification_eligible,
    )
    # When the source checkout that produced this run is present, recompute the entire schema binding,
    # including dirty bytes.  Historical bundles remain verifiable through their hash chain and Git
    # object fields; a current-run attacker cannot substitute one well-formed SHA for another.
    if (
        _full_git_sha() == expected_sha
        and _checkout_state_digest() == checkout_digest
        and manifest.get("schemaAuthority") != _schema_authority_document(expected_sha)
    ):
        raise EvidenceError("proof schema authority differs from the source checkout it claims")
    release_artifacts = _validate_release_artifacts(
        str(profile),
        manifest.get("releaseArtifacts"),
        expected_sha,
        eligible=certification_eligible,
    )
    if "windowsReleaseAuthority" not in manifest:
        raise EvidenceError("proof manifest omits its Windows release-authority binding state")
    windows_release_authority = _validate_windows_release_authority_binding(
        manifest.get("windowsReleaseAuthority"),
        proof_root=path.parent,
        manifest=manifest,
        expected_sha=expected_sha,
        eligible=certification_eligible,
        profile=str(profile),
    )
    if "rustCoveragePrerequisite" not in manifest:
        raise EvidenceError("proof manifest omits its mandatory Rust coverage prerequisite")
    rust_coverage_phase = _validate_rust_coverage_binding(
        manifest.get("rustCoveragePrerequisite"),
        proof_root=path.parent,
        expected_sha=expected_sha,
        expected_checkout_digest=checkout_digest,
        require_fresh=False,
    )
    _validate_model_attestation_binding(
        manifest.get("modelAttestation"),
        expected_sha,
        eligible=certification_eligible,
        profile=str(profile),
    )
    if manifest.get("knownDefectDigest") != _known_defect_digest(results, evidence_results):
        raise EvidenceError("proof known-defect digest is missing or cannot be reconstructed")

    if certification_eligible:
        required_roles = set(PROFILE_REQUIRED_RELEASE_ARTIFACT_ROLES[str(profile)])
        actual_roles = {str(artifact["role"]) for artifact in release_artifacts}
        if not required_roles <= actual_roles:
            raise EvidenceError("proof is marked certifying without all release-artifact roles")

    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list):
        raise EvidenceError("proof manifest has no artifact inventory")
    artifact_by_path: dict[str, dict[str, object]] = {}
    proof_root = path.parent.resolve()
    for artifact in artifacts:
        if not isinstance(artifact, dict) or not isinstance(artifact.get("path"), str):
            raise EvidenceError("proof manifest contains a malformed artifact")
        relative = Path(str(artifact["path"]))
        if relative.is_absolute() or str(relative) in {"", "."} or str(relative) in artifact_by_path:
            raise EvidenceError("proof manifest contains an unsafe or duplicate artifact path")
        candidate = (path.parent / relative).resolve()
        try:
            candidate.relative_to(proof_root)
        except ValueError as error:
            raise EvidenceError(f"proof artifact escapes its run directory: {relative}") from error
        expected_bytes = artifact.get("bytes")
        if (
            not candidate.is_file()
            or not isinstance(expected_bytes, int)
            or isinstance(expected_bytes, bool)
            or expected_bytes < 0
            or candidate.stat().st_size != expected_bytes
            or sha256_file(candidate) != artifact.get("sha256")
        ):
            raise EvidenceError(f"proof artifact is missing or changed: {candidate}")
        artifact_by_path[str(relative)] = artifact
    required_artifacts = {
        "events.jsonl",
        "gate-registry.json",
        "environment.json",
        RUN_AUTHORITY_NAME,
        EVIDENCE_CONTRACT_NAME,
        "STATUS.md",
    }
    if not required_artifacts <= set(artifact_by_path):
        raise EvidenceError("proof artifact inventory omits a required run authority")
    if windows_release_authority is not None and artifact_by_path.get(
        WINDOWS_RELEASE_AUTHORITY_NAME
    ) != manifest.get("windowsReleaseAuthority"):
        raise EvidenceError(
            "proof Windows release-authority binding differs from the global artifact inventory"
        )
    rust_coverage_binding = manifest["rustCoveragePrerequisite"]
    if artifact_by_path.get(str(rust_coverage_binding["path"])) != {
        "path": rust_coverage_binding["path"],
        "sha256": rust_coverage_binding["sha256"],
        "bytes": rust_coverage_binding["bytes"],
    }:
        raise EvidenceError(
            "proof Rust coverage prerequisite manifest differs from the global artifact inventory"
        )
    actual_inventory = {
        str(candidate.relative_to(path.parent))
        for candidate in path.parent.rglob("*")
        if candidate.is_file()
        and candidate not in {path.parent / "manifest.json", path.parent / PRODUCT_ATTESTATION_NAME}
    }
    if actual_inventory != set(artifact_by_path):
        raise EvidenceError("proof artifact inventory is not an exact image of the run directory")
    for artifact in result_artifacts:
        artifact_path = artifact.get("path") if isinstance(artifact, dict) else None
        if not isinstance(artifact_path, str) or artifact_by_path.get(artifact_path) != artifact:
            raise EvidenceError("gate artifact is absent from or differs from the global inventory")

    event_log = path.parent / "events.jsonl"
    try:
        events = [json.loads(line) for line in event_log.read_text(encoding="utf-8").splitlines() if line.strip()]
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"proof journal cannot be reconstructed: {error}") from error
    if not events or any(not isinstance(event, dict) for event in events):
        raise EvidenceError("proof journal is empty or malformed")
    for sequence, event in enumerate(events, start=1):
        if (
            event.get("schema") != 1
            or event.get("sequence") != sequence
            or event.get("runToken") != expected_token
            or not isinstance(event.get("event"), str)
        ):
            raise EvidenceError("proof journal sequence or identity is invalid")
    first, last = events[0], events[-1]
    coverage_completed = _parse_utc(
        rust_coverage_phase["endedAt"], "embedded coverage completion"
    )
    coverage_expires = _parse_utc(
        rust_coverage_phase["expiresAt"], "embedded coverage expiration"
    )
    proof_started = _parse_utc(first.get("at"), "proof run_start")
    proof_ended = _parse_utc(last.get("at"), "proof run_end")
    if proof_started < coverage_completed or proof_ended >= coverage_expires:
        raise EvidenceError(
            "proof run did not consume a freshly completed Rust coverage prerequisite"
        )
    if (
        first.get("event") != "run_start"
        or first.get("fullGitSha") != expected_sha
        or first.get("sourceTreeDigest") != expected_tree
        or first.get("checkoutStateDigest") != checkout_digest
        or first.get("profile") != profile
        or first.get("quick") != quick
        or first.get("gateRegistryHash") != manifest.get("gateRegistryHash")
        or first.get("authorityMode") != authority_mode
        or first.get("runAuthorityDigest") != run_authority_digest
    ):
        raise EvidenceError("proof journal has no matching run_start authority")
    if (
        last.get("event") != "run_end"
        or last.get("fullGitSha") != expected_sha
        or last.get("profile") != profile
        or last.get("exitCode") != reconstructed_code
        or last.get("verdict") != reconstructed_verdict
        or last.get("results") != len(results)
        or last.get("staleTakeover") != stale_takeover
        or last.get("authorityMode") != authority_mode
        or last.get("runAuthorityDigest") != run_authority_digest
        or last.get("diagnosticAuthorityOverrides")
        is not (authority_mode == AUTHORITY_MODE_DIAGNOSTIC)
    ):
        raise EvidenceError("proof journal has no matching terminal run_end")
    if sum(event.get("event") == "run_start" for event in events) != 1 or sum(
        event.get("event") == "run_end" for event in events
    ) != 1:
        raise EvidenceError("proof journal must contain exactly one start and one terminal event")
    abandonment_events = [event for event in events if event.get("event") == "abandonment"]
    if stale_takeover:
        if (
            len(abandonment_events) != 1
            or abandonment_events[0].get("abandonedRunToken") != abandoned_run_token
            or abandonment_events[0].get("reason") != "stale lease takeover"
        ):
            raise EvidenceError("proof stale-takeover manifest and journal disagree")
    elif abandonment_events:
        raise EvidenceError("clean proof run contains an unbound abandonment event")
    for result, gate in zip(results, expected_gates, strict=True):
        gate_events = [event for event in events if event.get("gate") == gate.id]
        starts = [event for event in gate_events if event.get("event") == "gate_start"]
        ends = [event for event in gate_events if event.get("event") == "gate_end"]
        if quick and gate.tier > 1:
            if starts or ends:
                raise EvidenceError(f"omitted quick gate {gate.id} unexpectedly executed")
        elif (
            len(starts) != 1
            or len(ends) != 1
            or ends[0].get("status") != result.get("status")
            or starts[0].get("environmentDigest") != result_environment_digests.get(gate.id)
            or ends[0].get("environmentDigest") != result_environment_digests.get(gate.id)
        ):
            raise EvidenceError(f"proof journal does not match result for gate {gate.id}")
    _validate_product_attestation(path.parent / PRODUCT_ATTESTATION_NAME, path, manifest)
    return manifest


def _profile_verdict(
    profile: str,
    quick: bool,
    results: list[tuple[str, str, float, str]],
    evidence_results: list[dict[str, object]] | None = None,
    *,
    stale_takeover: bool = False,
    diagnostic_authority_overrides: bool = False,
) -> tuple[int, str]:
    failures = [name for name, status, _, _ in results if status == FAIL]
    incomplete = [
        name
        for name, status, _, _ in results
        if status in {SKIP_ENV, NOT_BUILT, NOT_RUN_QUICK, PASS_AFTER_RETRY}
    ]
    if failures:
        verdict = f"RED — {len(failures)} gate(s) failed: {', '.join(failures)}"
        if diagnostic_authority_overrides:
            verdict += (
                " | DIAGNOSTIC — caller live-authority overrides were enabled; "
                "this run cannot certify"
            )
        return 1, verdict
    evidence_results = evidence_results if evidence_results is not None else _pending_evidence_results(profile)
    blockers = [
        evidence["classId"]
        for evidence in evidence_results
        if evidence.get("status") != "VERIFIED"
    ]
    if quick or incomplete or blockers or stale_takeover or diagnostic_authority_overrides:
        reasons = []
        if incomplete:
            reasons.append(f"non-certifying gates: {', '.join(incomplete)}")
        if blockers:
            reasons.append(f"required evidence pending: {'; '.join(blockers)}")
        if quick:
            reasons.append("quick mode omitted required tiers")
        if stale_takeover:
            reasons.append(
                "stale-lock takeover occurred; this recovery run is diagnostic and a fresh no-takeover run is required"
            )
        if diagnostic_authority_overrides:
            reasons.append(
                "caller live-authority overrides were enabled; this run is permanently diagnostic and cannot certify"
            )
        return 2, "INCOMPLETE — " + " | ".join(reasons)
    final = {
        PROFILE_OWNER: "CORTEX PRODUCT 10/10 — OWNER WORKSTATION",
        PROFILE_WINDOWS: "CORTEX PRODUCT 10/10 — WINDOWS 11",
        PROFILE_MODEL: "CORTEX MODEL EVIDENCE — VERIFIED",
        PROFILE_FULL: "CORTEX 10/10: ALL GATES GREEN",
    }
    return 0, final[profile]


def aggregate_main(
    quick: bool,
    status_md: str | None,
    profile: str,
    *,
    diagnostic_live_authority_overrides: bool = False,
) -> int:
    run_authority = _prepare_run_authority(diagnostic_live_authority_overrides)
    authority_mode, run_authority_digest = _validate_run_authority(run_authority)
    full_sha = _full_git_sha()
    source_tree_digest = _source_tree_digest()
    checkout_state_digest = _checkout_state_digest()
    run_token = uuid.uuid4().hex
    run_dir = PROOF_ROOT / run_token
    run_dir.mkdir(parents=True, exist_ok=False)
    journal = EvidenceJournal(run_dir / "events.jsonl", run_token)
    lease = LeaseManager(RUN_LOCK, full_sha, profile, run_token)
    registry = gate_registry_document()
    registry_hash = gate_registry_hash()
    environment = _environment_document()
    evidence_contract = evidence_contract_document()
    evidence_results = _pending_evidence_results(profile)
    stale_takeover = False
    abandoned_run_token: str | None = None
    atomic_write_json(run_dir / "gate-registry.json", registry)
    atomic_write_json(run_dir / "environment.json", environment)
    atomic_write_json(run_dir / RUN_AUTHORITY_NAME, run_authority)
    atomic_write_json(run_dir / EVIDENCE_CONTRACT_NAME, evidence_contract)
    results: list[tuple[str, str, float, str]] = []
    result_documents: list[dict[str, object]] = []
    rust_coverage_prerequisite: dict[str, object] | None = None
    journal_finalized = False
    try:
        if LEGACY_RUN_LOCK.exists():
            try:
                legacy_pid = int(LEGACY_RUN_LOCK.read_text(encoding="utf-8").strip().split()[0])
            except (OSError, UnicodeError, ValueError, IndexError) as error:
                raise LeaseError(f"unknown legacy verifier lock identity: {error}") from error
            if _pid_alive(legacy_pid):
                raise LeaseError(
                    f"legacy verifier pid {legacy_pid} is live but has no creation-time/token identity; "
                    "takeover fails closed"
                )
            LEGACY_RUN_LOCK.unlink()
        with acquired_lease(lease) as abandoned_token:
            stale_takeover = abandoned_token is not None
            abandoned_run_token = abandoned_token
            journal.append(
                "run_start",
                fullGitSha=full_sha,
                sourceTreeDigest=source_tree_digest,
                checkoutStateDigest=checkout_state_digest,
                profile=profile,
                quick=quick,
                gateRegistryHash=registry_hash,
                authorityMode=authority_mode,
                runAuthorityDigest=run_authority_digest,
            )
            if abandoned_token is not None:
                journal.append("abandonment", abandonedRunToken=abandoned_token, reason="stale lease takeover")
            rust_coverage_prerequisite = _consume_rust_coverage_prerequisite(
                run_dir,
                expected_sha=full_sha,
                expected_checkout_digest=checkout_state_digest,
            )
            selected = [gate for gate in GATES if profile in gate.profiles]
            for gate in selected:
                _assert_source_state(full_sha, source_tree_digest, checkout_state_digest)
                if quick and gate.tier > 1:
                    result = (gate.id, NOT_RUN_QUICK, 0.0, "quick mode")
                    results.append(result)
                    result_documents.append(
                        {
                            "gateId": gate.id,
                            "status": NOT_RUN_QUICK,
                            "seconds": 0.0,
                            "detail": "quick mode",
                            "environmentAuthority": None,
                        }
                    )
                    continue
                print(f"\n----- [tier {gate.tier}] {gate.id} :: {gate.charter_ref}", flush=True)
                status, seconds, detail, artifacts, environment_authority = _run_gate_worker(
                    gate,
                    run_dir,
                    run_token,
                    lease,
                    journal,
                    authority_mode=authority_mode,
                    run_authority_digest=run_authority_digest,
                )
                results.append((gate.id, status, seconds, detail))
                result_documents.append(
                    {
                        "gateId": gate.id,
                        "status": status,
                        "seconds": round(seconds, 3),
                        "detail": detail,
                        "artifacts": artifacts,
                        "environmentAuthority": environment_authority,
                    }
                )
                print(f"  => {status} {gate.id} {seconds:.1f}s", flush=True)
                if detail:
                    print(f"     {detail}", flush=True)
                _assert_source_state(full_sha, source_tree_digest, checkout_state_digest)

            evidence_results = _derive_evidence_results(
                profile,
                result_documents,
                run_dir,
                full_sha,
            )
            code, verdict = _profile_verdict(
                profile,
                quick,
                results,
                evidence_results,
                stale_takeover=stale_takeover,
                diagnostic_authority_overrides=(
                    authority_mode == AUTHORITY_MODE_DIAGNOSTIC
                ),
            )
            # Status is an immutable proof artifact, not a tracked source file. Writing docs/STATUS.md
            # made the certified SHA self-invalidating: committing the generated SHA changed the SHA,
            # and every same-commit rerun dirtied the checkout. Preserve the CLI option only as an
            # explicit migration notice; no external status target is authoritative or mutated.
            if status_md:
                print(
                    f"\n[status-md] external target retired ({status_md}); wrote proof-local STATUS.md instead",
                    flush=True,
                )
            write_status_md(
                run_dir / "STATUS.md",
                full_sha,
                quick,
                results,
                verdict,
                profile,
                evidence_results,
            )
            _assert_source_state(full_sha, source_tree_digest, checkout_state_digest)
            journal.append(
                "run_end",
                fullGitSha=full_sha,
                profile=profile,
                exitCode=code,
                verdict=verdict,
                results=len(results),
                staleTakeover=stale_takeover,
                authorityMode=authority_mode,
                runAuthorityDigest=run_authority_digest,
                diagnosticAuthorityOverrides=(authority_mode == AUTHORITY_MODE_DIAGNOSTIC),
            )
            journal_finalized = True
            manifest_path = run_dir / "manifest.json"
            schema_catalog_bytes = MIGRATION_CATALOG.read_bytes()
            schema_proof_path = run_dir / "source-authority" / "migrations.rs"
            schema_proof_path.parent.mkdir(parents=True, exist_ok=False)
            atomic_write_bytes(schema_proof_path, schema_catalog_bytes)
            schema_authority = _schema_authority_document(full_sha, schema_catalog_bytes)
            release_artifacts = _release_artifact_bindings(full_sha)
            windows_release_authority = _persist_windows_release_authority(run_dir)
            model_attestation = _model_attestation_binding()
            _validate_rust_coverage_binding(
                rust_coverage_prerequisite,
                proof_root=run_dir,
                expected_sha=full_sha,
                expected_checkout_digest=checkout_state_digest,
                require_fresh=True,
            )
            manifest = {
                "schema": 1,
                "complete": True,
                "runToken": run_token,
                "fullGitSha": full_sha,
                "sourceTreeDigest": source_tree_digest,
                "checkoutStateDigest": checkout_state_digest,
                "profile": profile,
                "quick": quick,
                "environment": environment,
                "runAuthority": run_authority,
                "gateRegistryHash": registry_hash,
                "evidenceContractHash": evidence_contract_hash(),
                "certificationEvidence": evidence_results,
                "results": result_documents,
                "verdict": verdict,
                "exitCode": code,
                "certificationEligible": code == 0,
                "requiredEvidencePending": [
                    result["classId"]
                    for result in evidence_results
                    if result["status"] != "VERIFIED"
                ],
                "schemaAuthority": schema_authority,
                "releaseArtifacts": release_artifacts,
                "windowsReleaseAuthority": windows_release_authority,
                "rustCoveragePrerequisite": rust_coverage_prerequisite,
                "modelAttestation": model_attestation,
                "knownDefectDigest": _known_defect_digest(result_documents, evidence_results),
                "staleTakeover": {
                    "occurred": stale_takeover,
                    "abandonedRunToken": abandoned_run_token,
                },
                "artifacts": _manifest_artifacts(run_dir),
            }
            atomic_write_json(manifest_path, manifest)
            attestation_path = run_dir / PRODUCT_ATTESTATION_NAME
            atomic_write_json(attestation_path, _product_attestation_document(manifest_path, manifest))
            _validate_completed_manifest(
                manifest_path,
                full_sha,
                run_token,
                require_current_live_authority=(authority_mode == AUTHORITY_MODE_LIVE),
            )
            _assert_source_state(full_sha, source_tree_digest, checkout_state_digest)
            pointer = {
                "schema": 1,
                "runToken": run_token,
                "fullGitSha": full_sha,
                "profile": profile,
                "manifest": os.path.relpath(manifest_path, LATEST_PROOF.parent),
                "manifestSha256": sha256_file(manifest_path),
                "productAttestation": os.path.relpath(attestation_path, LATEST_PROOF.parent),
                "productAttestationSha256": sha256_file(attestation_path),
            }
            publish_validated_json(
                LATEST_PROOF,
                pointer,
                lambda candidate: _validate_latest_proof(candidate, full_sha),
            )
            print(f"\nVERDICT: {verdict}", flush=True)
            print(f"proof: {manifest_path}", flush=True)
            return code
    except KeyboardInterrupt:
        with contextlib.suppress(EvidenceError):
            if journal_finalized:
                journal.append(
                    "publication_failure",
                    fullGitSha=full_sha,
                    profile=profile,
                    reason="KeyboardInterrupt after run_end",
                )
            else:
                journal.append("run_end", fullGitSha=full_sha, profile=profile, exitCode=130, verdict="ABORTED")
        print("\nVERDICT: ABORTED — no proof pointer published", flush=True)
        return 130
    except (EvidenceError, LeaseError, OSError, ValueError) as error:
        with contextlib.suppress(EvidenceError):
            if journal_finalized:
                # A run_end written before manifest/attestation publication is not a completed run.
                # Appending this event makes the journal non-terminal and preserves the true failure;
                # no pointer is published (or any post-write pointer becomes validation-invalid).
                journal.append(
                    "publication_failure",
                    fullGitSha=full_sha,
                    profile=profile,
                    exitCode=1,
                    verdict="VERIFIER FAILURE",
                    detail=str(error),
                )
            else:
                journal.append("run_end", fullGitSha=full_sha, profile=profile, exitCode=1, verdict="VERIFIER FAILURE", detail=str(error))
        print(f"\nVERDICT: VERIFIER FAILURE — {error}", flush=True)
        return 1


LEGACY_RUN_LOCK = LOG_DIR / "verify10.lock"


def _pid_alive(pid: int) -> bool:
    """True if a process with this PID exists. Windows has no os.kill(pid, 0) semantics worth trusting."""
    if sys.platform == "win32":
        out = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/NH"], capture_output=True, text=True, errors="replace"
        )
        return str(pid) in out.stdout
    try:
        os.kill(pid, 0)
    except (ProcessLookupError, PermissionError) as e:
        return isinstance(e, PermissionError)  # EPERM means it exists but is not ours
    return True


@contextlib.contextmanager
def _retired_single_instance():
    """Refuse to start while another sweep is already running.

    WHY. Two sweeps in flight corrupt each other and the record, and it happened TWICE on 2026-08-03:

      * They fight over the same fixed debug ports (9271/9333/9334/9335 ...). The loser's probe hits
        `PRECONDITION FAILED: debug port already answering` and dies — a leg failing for a reason that
        has nothing to do with the code. Three empty `cortex-egress-*` profiles at 10:16:34 were exactly
        this, and went unexplained for hours.
      * `docs/STATUS.md` is stamped with HEAD at WRITE time, not at run start. The earlier run therefore
        labelled its verdict with a commit it had never tested — a green attributed to the wrong code,
        which is precisely the kind of claim this whole repo exists to prevent.

    Refusing is not a pass: it exits 2 (INCOMPLETE), never 0. A stale lock (the PID is gone — killed
    run, crash) is taken over rather than blocking forever, because a gate nobody can start is its own
    outage.
    """
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    if LEGACY_RUN_LOCK.exists():
        try:
            holder = int(LEGACY_RUN_LOCK.read_text(encoding="utf-8").strip().split()[0])
        except (ValueError, OSError, IndexError):
            holder = -1
        ours = holder == os.getpid()
        if holder > 0 and not ours and _pid_alive(holder):
            print(
                f"\nREFUSING TO START: another verify-10 sweep is already running (pid {holder}).\n"
                f"  Two sweeps fight over the same debug ports and both write docs/STATUS.md, which is\n"
                # ASCII only: the Windows console renders an em-dash as a replacement character, and a
                # refusal message somebody reads mid-run must not be mojibake.
                f"  stamped with HEAD at write time (not run start), so the earlier one would label its\n"
                f"  verdict with a commit it never tested. Wait for it, or stop it, then re-run.\n"
                f"  Lock: {LEGACY_RUN_LOCK}",
                flush=True,
            )
            sys.exit(2)
        # Only call it STALE when it actually is. Re-entering under our own pid is not a dead holder,
        # and a message that says otherwise is a small lie in the one place someone looks when the
        # gate behaves oddly.
        if not ours:
            print(f"(taking over a stale verify-10 lock from dead pid {holder})", flush=True)
    LEGACY_RUN_LOCK.write_text(f"{os.getpid()}\n", encoding="utf-8")
    try:
        yield
    finally:
        # Only remove OUR lock: a takeover race must not delete the winner's.
        try:
            if LEGACY_RUN_LOCK.exists() and LEGACY_RUN_LOCK.read_text(encoding="utf-8").strip().split()[0] == str(os.getpid()):
                LEGACY_RUN_LOCK.unlink()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--static", action="store_true", help="historical 4-gate governance check (CI contract)")
    ap.add_argument("--quick", action="store_true", help="tiers 0-1 only")
    ap.add_argument("--profile", choices=sorted(PROFILES), default=PROFILE_OWNER)
    ap.add_argument(
        "--status-md",
        metavar="PATH",
        default=None,
        help="retired compatibility option; status is always written inside the immutable proof run",
    )
    ap.add_argument("--gate-worker", help=argparse.SUPPRESS)
    ap.add_argument("--worker-result", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--run-token", help=argparse.SUPPRESS)
    ap.add_argument("--authority-mode", choices=sorted(AUTHORITY_MODES), help=argparse.SUPPRESS)
    ap.add_argument("--run-authority-digest", help=argparse.SUPPRESS)
    ap.add_argument(
        "--diagnostic-live-authority-overrides",
        action="store_true",
        help=(
            "allow caller CORTEX_DB/data/release overrides into live gates for diagnosis; "
            "the immutable run is permanently non-certifying"
        ),
    )
    ap.add_argument(
        "--rust-coverage-prerequisite",
        action="store_true",
        help="run the separately supervised no-retry Rust coverage prerequisite and publish only its immutable completed pointer",
    )
    ap.add_argument(
        "--require-certifying-proof",
        action="store_true",
        help="consume latest-proof for the exact current SHA/profile and exit nonzero unless it is certifying",
    )
    ap.add_argument(
        "--proof-manifest",
        type=Path,
        help="consume this detached immutable manifest.json instead of the workstation latest-proof pointer",
    )
    ap.add_argument("--expected-sha", help="exact 40-hex release commit required from a detached proof")
    ap.add_argument("--windows-release-bundle", type=Path)
    ap.add_argument("--expected-repository", help="exact owner/repository identity for GitHub provenance")
    ap.add_argument("--expected-ref", help="exact refs/tags/v... identity for GitHub provenance")
    ap.add_argument("--expected-version", help="exact stable package version without the v prefix")
    ap.add_argument("--windows-signer-thumbprint", help="expected SHA-1 Authenticode signer thumbprint")
    ap.add_argument("--windows-signer-cert-sha256", help="expected SHA-256 signer certificate fingerprint")
    args = ap.parse_args()
    if args.rust_coverage_prerequisite:
        if (
            args.static
            or args.quick
            or args.gate_worker
            or args.require_certifying_proof
            or args.proof_manifest is not None
            or args.expected_sha is not None
            or args.windows_release_bundle is not None
            or args.diagnostic_live_authority_overrides
            or args.authority_mode is not None
            or args.run_authority_digest is not None
        ):
            ap.error("--rust-coverage-prerequisite cannot be combined with another verifier mode")
        return rust_coverage_prerequisite_main()
    if args.proof_manifest is not None and not args.require_certifying_proof:
        ap.error("--proof-manifest requires --require-certifying-proof")
    if args.expected_sha is not None and args.proof_manifest is None:
        ap.error("--expected-sha is accepted only with --proof-manifest")
    if args.diagnostic_live_authority_overrides and (
        args.static
        or args.gate_worker
        or args.require_certifying_proof
        or args.proof_manifest is not None
    ):
        ap.error(
            "--diagnostic-live-authority-overrides is accepted only for a full or quick verifier run"
        )
    if not args.gate_worker and (
        args.authority_mode is not None or args.run_authority_digest is not None
    ):
        ap.error("internal worker authority options are accepted only with --gate-worker")
    release_options = (
        args.expected_repository,
        args.expected_ref,
        args.expected_version,
        args.windows_signer_thumbprint,
        args.windows_signer_cert_sha256,
    )
    if args.windows_release_bundle is None and any(value is not None for value in release_options):
        ap.error("Windows release identity options require --windows-release-bundle")
    release_authority: dict[str, object] | None = None
    if args.windows_release_bundle is not None:
        if any(not isinstance(value, str) or not value for value in release_options):
            ap.error(
                "--windows-release-bundle requires repository, ref, version, signer thumbprint, and signer certificate SHA-256"
            )
        release_sha = args.expected_sha or _full_git_sha()
        try:
            release_authority = configure_windows_release_authority(
                args.windows_release_bundle,
                expected_sha=release_sha,
                expected_repository=args.expected_repository,
                expected_ref=args.expected_ref,
                expected_version=args.expected_version,
                signer_thumbprint=args.windows_signer_thumbprint,
                signer_cert_sha256=args.windows_signer_cert_sha256,
                require_windows_product=(
                    args.require_certifying_proof
                    and args.profile in {PROFILE_WINDOWS, PROFILE_FULL}
                ),
            )
        except (OSError, ReleaseBundleError, EvidenceError, subprocess.SubprocessError) as error:
            print(f"CORTEX RELEASE BUNDLE REJECTED: {error}", flush=True)
            return 1
    if args.require_certifying_proof:
        if args.static or args.quick or args.gate_worker:
            ap.error("--require-certifying-proof cannot be combined with a verifier run mode")
        try:
            if args.proof_manifest is not None:
                if args.expected_sha is None or not re.fullmatch(r"[0-9a-f]{40}", args.expected_sha):
                    ap.error("detached proof consumption requires --expected-sha as exact lowercase 40-hex")
                manifest = _require_detached_certifying_proof(
                    args.proof_manifest,
                    args.profile,
                    args.expected_sha,
                    release_authority,
                )
            else:
                manifest = _require_latest_certifying_proof(
                    LATEST_PROOF,
                    args.profile,
                    release_authority=release_authority,
                )
        except (EvidenceError, OSError, ValueError) as error:
            print(f"CORTEX PROOF REJECTED: {error}", flush=True)
            return 1
        print(
            f"CORTEX PROOF ACCEPTED: {manifest['verdict']} @ {manifest['fullGitSha']}",
            flush=True,
        )
        return 0
    if args.gate_worker:
        if (
            args.worker_result is None
            or not args.run_token
            or args.authority_mode is None
            or not args.run_authority_digest
        ):
            ap.error(
                "--gate-worker requires --worker-result, --run-token, --authority-mode, and --run-authority-digest"
            )
        return gate_worker_main(
            args.gate_worker,
            args.worker_result,
            args.run_token,
            args.authority_mode,
            args.run_authority_digest,
        )
    if args.static:
        # The static governance check runs no legs, opens no ports and writes no STATUS.md, so it is
        # not what the lock protects against and must stay runnable alongside a sweep.
        static_main()
        return 0
    return aggregate_main(
        quick=args.quick,
        status_md=args.status_md,
        profile=args.profile,
        diagnostic_live_authority_overrides=args.diagnostic_live_authority_overrides,
    )


if __name__ == "__main__":
    sys.exit(main())
