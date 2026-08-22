#!/usr/bin/env python3
"""CORTEX verify-10 — the personal-use full-charter gate aggregator.

Self-locating: all paths resolve relative to the repository root (the parent of
this script's directory), so the gate runs identically from any working
directory and from CI (`python "$GITHUB_WORKSPACE/scripts/verify_10.py" --static`).

Modes
-----
  (default)  full aggregator: Tier 0 static governance, Tier 1 CI-equivalent
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
  GREEN — PERSONAL-USE SHIP-READY (exit 0) — every kept gate passed; the
                         owner-descoped/owner-gated tail is printed alongside.
  CORTEX 10/10: ALL GATES GREEN (exit 0) — only possible when nothing is
                         descoped or owner-gated: per SHIP_FINAL_PLAN #58 this
                         can only ever happen after the P7 re-audit.

Owner amendment 2026-07-10: "ship" = the owner's PERSONAL USE, truly reliable
and bug-free. Distribution legs are descoped (printed below, never dropped);
no honesty/privacy/reliability/correctness gate is waived.
"""
import argparse
import contextlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
APP = REPO_ROOT / "cortex-speech-app"
SRC_TAURI = APP / "src-tauri"
MANIFEST = SRC_TAURI / "Cargo.toml"
EXE = SRC_TAURI / "target" / "release" / "cortex-speech-app.exe"

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
    status = subprocess.run(["gh", "auth", "status"], capture_output=True, text=True)
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

PASS, FAIL, SKIP_ENV, NOT_BUILT = "PASS", "FAIL", "SKIP-ENV", "NOT-BUILT"
# --quick deliberately does not run tier-2/3 kept gates; they are counted with this status so the
# verdict is at best INCOMPLETE. Quick mode must never print the ship-ready GREEN line — that
# verdict was previously reachable ONLY in the least-verified mode (true-10 sweep 2026-07-11).
NOT_RUN_QUICK = "NOT-RUN-QUICK"


def _probe_deny():
    if shutil.which("cargo-deny"):
        return None
    return "cargo-deny not installed (cargo install cargo-deny)"


def _probe_exe():
    if EXE.exists():
        return None
    return "release exe missing - run `make build-app`"


def _probe_real_e2e():
    if not EXE.exists():
        return "release exe missing - run `make build-app`"
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
    if not EXE.exists():
        return "release exe missing - run `make build-app`"
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
        return f"7B server answered on {port} but its health reply is unparseable ({exc}) — NOT the champion protocol"
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
                f"7B server on {port} serves deployment {str(served)[:12]}… but the live champion pin is "
                f"{pinned[:12]}… — the WRONG MODEL is answering the champion port"
            )
    return None


def _probe_egress():
    if not EXE.exists():
        return "release exe missing - run `make build-app`"
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


def _fuzz_cmd(argstr):
    """`cargo +nightly fuzz <argstr>` — natively, or through a WSL login shell on Windows."""
    if sys.platform == "win32":
        return ["wsl", "--", "bash", "-lc", f"cd {_wsl_path(SRC_TAURI)} && cargo +nightly fuzz {argstr}"]
    return ["cargo", "+nightly", "fuzz", *argstr.split()]


def _fn_fuzz_smoke():
    """30s smoke per fuzz target; PASS only if EVERY target actually ran and was crash-free."""
    lst = subprocess.run(_fuzz_cmd("list"), capture_output=True, text=True)
    targets = [t for t in lst.stdout.split() if t]
    # Fail LOUD on an empty target list. A run that enumerates nothing would otherwise sail
    # through the loop below and return True — a vacuous pass, which is exactly the class of
    # dishonesty this repo's charter forbids. (Hit for real on 2026-07-26 when a non-login shell
    # left cargo off PATH: 0 targets, 0 iterations, "all clean".)
    if lst.returncode != 0 or not targets:
        print("  [ERR] cargo fuzz list failed or found no targets - refusing to report a pass")
        return False
    print(f"  {len(targets)} targets: {', '.join(targets)}")
    for t in targets:
        r = subprocess.run(_fuzz_cmd(f"run {t} -- -max_total_time=30"), capture_output=True, text=True)
        print(f"  fuzz {t}: {'ok' if r.returncode == 0 else 'CRASH/FAIL'}")
        if r.returncode != 0:
            for line in (r.stderr or r.stdout).splitlines()[-10:]:
                print(f"    {line}")
            return False
    return True


# (name, tier, kind, payload, cwd, env_probe, charter_ref)
#   kind "fn"  -> payload is a callable returning bool
#   kind "cmd" -> payload is a shell command string
GATES = [
    # Tier 0 — static governance (seconds)
    ("manifest-alignment", 0, "fn", check_manifests, None, None, "Git+integrity: versions byte-equal CHANGELOG"),
    ("repo-integrity", 0, "fn", check_repo_integrity, None, None, "Git+integrity: LICENSE/NOTICE/repo URL"),
    ("required-files", 0, "fn", check_required_files, None, None, "Engineering rigor: SECURITY.md/CODEOWNERS present"),
    ("ledger-schema", 0, "fn", check_provenance_ledger, None, None, "Data governance: ledger schema-valid"),
    ("license-compat", 0, "fn", check_license_compatibility, None, None, "Data governance: contamination gate"),
    # Tier 1 — CI-equivalent code gates (minutes)
    ("branch-protection", 1, "fn", check_branch_protection, None, _probe_branch_protection, "Git+integrity: main is protected on the remote, admins included (was OWNER_GATED item 49 - clicks done 2026-08-08, now machine-verified every sweep)"),
    ("python-policies", 1, "cmd", "npm run test:python-policies", APP, None, "honesty/privacy/CI/dataset policy tests"),
    ("spot-check-pool", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_spot_check_pool.py"}"', APP, None, "The listening-QC must cover the WHOLE accessible paid-review campaign, not merely be able to fire once. The gate mirrors live focus, roster, dialect, on-disk audio, prior per-reviewer scores, and the Rust queue/check cadence; it derives each reviewer's worst-case key requirement because no enforced quota prevents one eligible reviewer from draining the queue. MEASURED 2026-08-21: the Hawleri campaign exposed 1,293 work clips but only 0-2 fresh keys per reviewer, so the old floor-of-3 gate would have gone green after three owner edits and then silently stopped measuring. Answer keys must be genuine owner-adjudicated/is_gold rows; never synthetic."),
    ("dataset-duplicates", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_dataset_duplicates.py"}"', APP, None, "The same-recording-under-different-names audit, on the LIVE library. FOUND BY THE OWNER'S EARS 2026-08-17, not by any gate: one recording lived under three filenames as different ENCODES, so the byte fingerprint saw three distinct files — ~68 duplicate sentences entered the corpus and 33 were reviewed (paid) twice, and duplicate content across nominally-different recordings can straddle a train/test split. Signal: source-timeline offset AND transcript agreeing across different files. Baseline 70, ratchets DOWN only."),
    ("snapshot-immutability", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_snapshot_immutability.py"}"', APP, None, "Gate C of docs/PLAN_TRUE_10.md. A training run cites a dataset snapshot id, and every CER measured from the resulting model hangs off that citation. This proves, on the LIVE library, that the id IS the content hash of the manifest it sealed (not a label someone chose), that no id is reused, that the sealed config names its own id, and that any pack still on disk still hashes to the snapshot it claims. Without it, 'trained on snapshot X' is decoration and every number downstream is unanchored. SKIP-ENV until the first pack is exported — it reports on data that exists and never invents a pass."),
    ("challenger-loop", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_challenger_loop.py"}"', APP, None, "Gate D of docs/PLAN_TRUE_10.md. The retrain flywheel's danger is not that a challenger LOSES — a REJECT is a good outcome and passes this gate — it is a run that LOOKS finished: a record saying 'trained' for training that never happened, a verdict with no snapshot behind it, or a PROMOTE whose own numbers do not support it. Checks the chain (train_challenger / build_eval_slices / promotion_gate) is present and audits every run record and verdict on disk for internal consistency. SKIP-ENV until a canary has actually run: wiring is not evidence, and a gate that says OK for an unrun loop is the flattering kind."),
    ("reviewer-queues-live", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_reviewer_queues_live.py"}"', APP, None, "Every reviewer holding a live link has clips they are ALLOWED to review. MEASURED 2026-08-17: two independent bugs made five of eight reviewers' queues empty while the owner was paying them, and each hid the other. The 1,031 recovered clips were relinked into D:\\Kurdish Corpora\\sorani\\ZarPodcast while dialect.rs still mapped only their pre-recovery path, so they were UNMAPPED and the dialect check fails closed; meanwhile the roster file carried a \"_comment\" string, which a strict HashMap<String, Vec<String>> parse rejects outright, and that failure path is \"unrestricted\" — so the protection was simultaneously off for everyone. Every row, every JSON file and every Rust function read correctly in isolation; only computing what each NAMED reviewer would actually be served exposes it. supervision-live cannot: the server answers 200 for an empty queue."),
    ("review-serving-provenance", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_serving_provenance.py"}"', APP, None, "Honesty at the SERVING path, on the LIVE db: annotated_transcript is human-only, and every untouched clip serves the champion's own transcript. MEASURED 2026-08-12: 348 rows held machine text in the human field, so the phone review page served a stale paraphrase while the fresh champion drafts sat invisible — reviewers corrected words the speaker never said. Write-path checks passed the whole time; only reading the row the server actually serves catches this class."),
    ("typecheck", 1, "cmd", "npm run typecheck", APP, None, "svelte-check + tsc"),
    ("lint-js", 1, "cmd", "npm run lint", APP, None, "eslint"),
    ("clippy", 1, "cmd", f'cargo clippy --manifest-path "{MANIFEST}" --all-targets -- -D warnings', REPO_ROOT, None, "Engineering rigor: clippy -D warnings"),
    ("fmt-check", 1, "cmd", f'cargo fmt --manifest-path "{MANIFEST}" --all -- --check', REPO_ROOT, None, "rustfmt"),
    ("runtime-asset-integrity", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "fetch_models.py"}" --check', APP, None, "SHA-256 of every required runtime-support asset plus every optional ASR artifact already present. Missing optional 300M/1B/MMS is healthy; a partial or mismatched optional installation is RED. The externally served WSL7B identity is proven separately at the serving path."),
    ("test-frontend", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "assert_ran.py"}" --min 200 --kind vitest -- npm test', APP, None, "vitest, with a floor. MEASURED 2026-08-03: vitest exits 0 when it matches ZERO tests, so broken discovery (a stray -t, an include pattern that stops matching) would have read as a clean pass. Floor 200 against a real 217; assert_ran also FAILS if it cannot find the count line at all, because a guard that silently stops understanding its input is worse than none."),
    ("test-rust", 1, "cmd", f'"{sys.executable}" "{APP / "scripts" / "assert_ran.py"}" --min 1100 --kind cargo -- cargo test --manifest-path "{MANIFEST}" --jobs 4', REPO_ROOT, None, "Sorani goldens, wer-vs-jiwer, holdout hash, ONNX manifest, proof-metadata. MEASURED 2026-08-03: `cargo test` exits 0 on 'test result: ok. 0 passed; 1105 filtered out', so a cfg or filter that silently excluded the test tree would have read as a clean pass on the LARGEST leg. Floor 1100 against a real 1193 across 35 binaries."),
    ("audit", 1, "cmd", "npm audit --omit=dev && npm ls --all", APP, None, "npm supply chain. `npm ls --all` is the second half deliberately: MEASURED 2026-08-06, `npm audit` reported 0 vulnerabilities while the INSTALLED tree was structurally invalid (ELSPROBLEMS: a hoisted picomatch@2 could not satisfy the `^3 || ^4` peer fdir asks for). A clean audit says 'no KNOWN CVE in what resolved'; it says nothing about whether the tree resolved correctly at all. Both halves, or the gate only proves half of supply chain."),
    ("deny", 1, "cmd", f'cargo deny --manifest-path "{MANIFEST}" check', REPO_ROOT, _probe_deny, "cargo supply chain"),
    ("test-e2e+a11y", 1, "cmd", "npm run test:e2e", APP, None, "A11y: axe WCAG 2.2 AA en+ckb/RTL (coverage assertion: WS2 follow-up)"),
    # Tier 2 — real binary on this machine (the personal-use core)
    ("database-integrity-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_database_integrity.py"}" --require-production-v58-repair', APP, None, "Whole LIVE SQLite truth, read-only and unskippable: quick_check and full integrity_check must each return exactly ok, foreign_key_check must return zero rows across every table, migration history must be exact, and the immutable v58 archives must prove the authorized 2,104+2,104 production repair by identity digest and provenance. Feature-specific gates cannot certify a database that is structurally healthy but missing its repair evidence."),
    ("review-compensation-readiness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_review_compensation_readiness.py"}"', APP, None, "Paid-review money truth on the LIVE database and active focus: the immutable policy must be exactly review-iqd-v1-2026-08-21 (edit 100%, unchanged accept 10%, valid reject 10%, skip 0% at 18,000 IQD/full-equivalent hour); every post-cutoff Couch event must have one exact durable ledger consequence; signed revisions, settlements, operation UUIDs, and canonical focused-work identities must balance. Missing migration/schema/evidence is RED, never skipped — source code containing migration 57 is not proof that the live database actually runs it."),
    ("reviewer-links-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_reviewer_links_live.py"}" --funnel --port 8737 --require-links --require-pilot', APP, None, "The exact Hawzhin and Pavel pilot credentials must authenticate through the advertised Tailscale Funnel and bind to the intended identities, database and fixed production port. The dedicated probe is read-only: it mints no cookie, evicts no phone session, leases no work and consumes no hidden-check key. Queue eligibility is independently proven by reviewer-queues-live. Public TLS verification remains enabled; missing Funnel/session/policy/links is RED, never skipped."),
    ("exe-freshness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_exe_freshness.py"}"', REPO_ROOT, _probe_exe, "Truth-in-advertising: exe compiled from HEAD"),
    ("playback-enforcement-readiness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_playback_enforcement_readiness.py"}" --exe "{EXE}"', APP, None, "Paid-review listening proof for the EXACT deployed binary: the build contains the refusal guard, at least 20 post-build phone decisions cover at least two reviewer browsers, and every landed decision carries >=85% playback evidence. No env probe and no --since override: a missing binary/database or an empty current-build window is RED, never skipped or backdated into a pass."),
    ("supervision-live", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_supervision_live.py"}"', REPO_ROOT, None, "Fitness to SERVE, not just to compile: the watchdog is enabled, every live reviewer link answers on 8737, and the data drive has room to write. MEASURED 2026-08-15: all three were false at once — CortexWatchdog left `Disabled` by the rebuild procedure, the app exited so five sent links were dead, and C: at 0 bytes had already broken the 10-minute DB snapshot ('periodic DB snapshot failed'). Every source-level gate was still capable of GREEN, because none of them looks at the machine."),
    ("real-app-e2e", 2, "cmd", f'node "{APP / "e2e_real_app.cjs"}"', APP, _probe_real_e2e, "Daily-use proof on a disposable profile: real exe + real audio + the exact pinned WSL7B champion + real transcript. CORTEX_GATE forces WSL7B, so an inherited diagnostic-engine override cannot weaken this gate."),
    # Tier 3 — deep proof legs (env-gated; skipped honestly when absent)
    ("egress-runtime", 3, "cmd", f'node "{APP / "scripts" / "egress_probe.cjs"}"', APP, _probe_egress, "Privacy: zero outbound TCP from the backend PID during startup + browse, with a positive-control sampler. Standard coverage makes no ASR-path claim and never auto-runs an installed smaller model. An explicit CORTEX_EGRESS_TRANSCRIBE=1 diagnostic adds WSL7B transcription coverage on a disposable profile."),
    ("champion-7b-preflight", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --jobs 4 -- --ignored wsl_7b_preflight', REPO_ROOT, _probe_champion_7b, "The champion's preflight against the REAL OmniASR-7B server. The champion drafts every clip (owner rule 2026-08-11), so the check that it is reachable before an import starts is the difference between a halt and a library half-drafted by a weaker engine."),
    # Deliberately count-agnostic: the gate enumerates targets with `cargo fuzz list` and fails loud on an
    # empty list, so hardcoding a number here only creates a second place to go stale. It said "5" until
    # the `features` target was removed with the dead FbankExtractor module it fuzzed (iteration 231).
    ("fuzz-smoke", 3, "fn", _fn_fuzz_smoke, None, _probe_fuzz, "Engineering rigor: every fuzz target, 0 crashes"),
    ("refinery-lift", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --test refinery_lift -- --ignored refinery_lift_injected_error_benchmark --nocapture', REPO_ROOT, None, "Refinery: >=30% CER reduction at <=15% escalation (fixed-seed injected-error benchmark, offline T0 path)"),
    ("fairness-gender-age", 3, "cmd", f'"{sys.executable}" "{APP / "scripts" / "fairness_gate.py"}"', REPO_ROOT, None, "WS4: gender/age CER disparity budget on committed corpus metadata (CORDI dialect leg owner-gated)"),
    ("pipeline-ipc-e2e", 3, "cmd", f'node "{APP / "e2e_pipeline_ipc.cjs"}"', APP, _probe_champion_ipc_harness, "Import -> VAD -> exact pinned WSL7B -> persisted transcript over real IPC on a disposable profile, independent of webview rendering."),
    ("heartbeat-runtime", 3, "cmd", f'node "{APP / "scripts" / "heartbeat_probe.cjs"}"', APP, _probe_ipc_harness, "Main-thread safety PROVEN AT RUNTIME: get_settings latency while slow commands run concurrently. The static test_command_main_thread_policy/test_ui_thread_blocking_audit pin the source shape; this measures the actual UI responsiveness they exist to protect."),
    ("bench-budget", 3, "cmd", f'"{sys.executable}" "{APP / "scripts" / "bench_gate.py"}"', APP, _probe_bench, "Criterion wall-clock regression budget against a COMMITTED baseline (docs/bench_baseline.json). The charter asks for this via github-action-benchmark on every PR; that CI clause is NOT satisfied here and stays open - this enforces the budget on the reference machine, where the charter's latency numbers are defined. Per-bench thresholds derived from measured run-to-run noise, and benches too noisy to gate are NAMED every run rather than given a pass-anything limit."),
    ("jobs-runtime", 3, "cmd", f'node "{APP / "scripts" / "jobs_probe.cjs"}"', APP, _probe_exe, "Durable Job Supervisor at runtime: a REAL export_dataset run is recorded in get_jobs and reaches 'succeeded' - the run_tracked bracketing proven end to end, not only in unit tests."),
    ("durability-drill", 3, "cmd", _drill_cmd("durability_writer", "durability_drill.py", "--cycles 25"), APP, None, "Crash durability PROVEN, not asserted: 25 hard kills of the real writer (production Database::open_with_retry + insert_segment) across write-phase and boot-phase, verifying integrity_check ok, zero LOST journaled edits, a contiguous id space and a row count that never decreases. The single reliability property daily review depends on - the app dying must never cost work that was saved. It existed and NOTHING ran it (found 2026-08-02 by asking which scripts no gate references); an unrun drill is a claim."),
    ("export-kill-drill", 3, "cmd", _drill_cmd("export_writer", "export_kill_drill.py", "--cycles 15"), APP, None, "Atomic-write design under real kills: 15 mid-export TerminateProcess cycles proving every JOURNALED export parses complete with the full row count, and that NO final .json is ever torn (atomic temp+fsync+rename in atomic_file.rs is the design under test). Scope honesty: process kill, not power loss. Same find as the durability drill - written, never run."),
]

# Charter DoD legs descoped by the owner amendment (2026-07-10) — always printed.
DESCOPED = [
    ("asosoft-600-eval-set", "Eval corpus: owner decision 2026-08-11 — AsoSoft publishes NO licence file and NO terms beyond \"research and non-commercial use\", and no contact address; evaluation rests on FLEURS ckb + CORDI (CC BY-SA 4.0) instead"),
    ("signed-installer", "Distribution: signtool verify /pa"),
    ("slsa-provenance", "Distribution: gh attestation verify"),
    ("signed-auto-updater", "Distribution (incl. updater clause of the egress bullet)"),
    ("store-install-paths", "Distribution: winget / Homebrew / Flathub"),
    ("hf-model-card", "Distribution: HF card + eval YAML + ethics section"),
    ("macos-notarization", "Distribution (was the explicit STRETCH leg)"),
    ("openssf-scorecard-check", "Engineering rigor: Scorecard >=8.0 required check"),
    ("signed-tag-protected-main", "Git+integrity: gitsign/Sigstore signed tag"),
]

# Kept, mandatory for full 10/10, waiting on the human — always printed.
OWNER_GATED = [
    ("iaa-kappa-ceiling", "item 44: recruit >=2 independent Sorani annotators"),
    ("cordi-dialect-fairness", "item 53: CORDI corpus agreement"),
    ("refinery-lift-in-product", "item 37: Gold Marathon (>=500 real review decisions)"),
]


LOG_DIR = Path(tempfile.gettempdir()) / "cortex-verify10"

# Append-only per-gate run record (external review 2026-08-06, P0.1): "a result that ran but cannot be
# retrieved is operationally indistinguishable from no result."
#
# The summary table is printed only after the LAST gate, so a caller that gave a ~40-minute sweep a
# ~30-minute timeout threw away every gate that had already passed — the work was done and the evidence
# was not. This file is written as each gate FINISHES, so a killed, timed-out or crashed run still leaves
# a durable, ordered record of exactly how far it got and what each leg cost.
#
# JSONL and append-only on purpose: a partial last line is the only damage a kill can do, and every line
# before it stays parseable. Best-effort — evidence bookkeeping must never be the thing that fails a
# sweep, so a write error is reported once and the run continues.
RUN_LOG = LOG_DIR / "runs.jsonl"
_run_log_broken = False


def record_run_event(**fields):
    """Append one JSON line to RUN_LOG. Never raises."""
    global _run_log_broken
    if _run_log_broken:
        return
    try:
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        with RUN_LOG.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(fields, ensure_ascii=False) + "\n")
            fh.flush()
            os.fsync(fh.fileno())  # a record that is still in a buffer when the process is killed is not a record
    except OSError as exc:
        _run_log_broken = True
        print(f"  (run-log unavailable: {exc} — the sweep continues; only its durable record is lost)")

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


def run_gate(name, kind, payload, cwd, probe, timeout=3600):
    """Run one gate; returns (status, seconds, detail). Full cmd output -> LOG_DIR/<gate>.log."""
    if probe:
        reason = probe()
        if reason:
            return SKIP_ENV, 0.0, reason
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
    try:
        r = subprocess.run(
            payload, shell=True, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
        # LNK1104 on system libs is a Windows file-lock (AV scan) flake, not a code failure:
        # retry exactly once, and say so — both attempts land in the log.
        if r.returncode != 0 and "LNK1104" in (r.stdout or "") + (r.stderr or ""):
            retried = " [retried once after LNK1104 linker file-lock flake]"
            r = subprocess.run(
                payload, shell=True, cwd=cwd, capture_output=True, text=True, timeout=timeout
            )
    except subprocess.TimeoutExpired:
        return FAIL, time.perf_counter() - t0, f"timed out after {timeout}s"
    secs = time.perf_counter() - t0
    # Persist the FULL output so every failure stays diagnosable after the run.
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{name}.log"
    log_path.write_text(
        f"$ {payload}\n(exit {r.returncode}, {secs:.1f}s)\n\n--- stdout ---\n{r.stdout or ''}"
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
                payload, shell=True, cwd=cwd, capture_output=True, text=True, timeout=timeout
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


def write_status_md(path, head, quick, results, verdict):
    """Emit the single generated source of truth for gate status.

    Hand-written docs that restate which gates pass go stale silently — OWNER_HANDOFF.md
    claimed `egress-runtime` was NOT-BUILT for weeks after it shipped, and refinery-lift
    needed the same manual correction. Under this repo's honesty law a doc asserting a
    gate state it did not measure is exactly the failure mode to design out, so docs now
    link here instead of restating.

    Deliberately deterministic: no timestamp, no per-gate timings. Two runs on the same
    commit produce a byte-identical file, so a re-run is a zero diff and a real diff
    always means a real change in gate state.
    """
    rows = "\n".join(f"| `{name}` | {status} |" for name, status, _, _ in results)
    descoped = "\n".join(f"| `{name}` | {why} |" for name, why in DESCOPED)
    gated = "\n".join(f"| `{name}` | {why} |" for name, why in OWNER_GATED)
    body = f"""<!-- GENERATED by scripts/verify_10.py --status-md. Do not hand-edit: your edit
     will be overwritten, and a hand-asserted gate state is precisely what this file exists
     to prevent. Regenerate with `python scripts/verify_10.py --status-md docs/STATUS.md`. -->

# Gate status — generated

**Commit:** `{head}` · **Mode:** {'quick (tiers 0-1)' if quick else 'full'}

**Verdict:** {verdict}

## Kept gates

| Gate | Status |
|---|---|
{rows}

## Owner-descoped (owner amendment 2026-07-10 — "ship" = personal use)

| Leg | Reason |
|---|---|
{descoped}

## Owner-gated, still pending

| Leg | Blocked on |
|---|---|
{gated}
"""
    Path(path).write_text(body, encoding="utf-8")
    print(f"\n[status-md] wrote {path}")


def aggregate_main(quick, status_md=None):
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


RUN_LOCK = LOG_DIR / "verify10.lock"


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
def single_instance():
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
    if RUN_LOCK.exists():
        try:
            holder = int(RUN_LOCK.read_text(encoding="utf-8").strip().split()[0])
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
                f"  Lock: {RUN_LOCK}",
                flush=True,
            )
            sys.exit(2)
        # Only call it STALE when it actually is. Re-entering under our own pid is not a dead holder,
        # and a message that says otherwise is a small lie in the one place someone looks when the
        # gate behaves oddly.
        if not ours:
            print(f"(taking over a stale verify-10 lock from dead pid {holder})", flush=True)
    RUN_LOCK.write_text(f"{os.getpid()}\n", encoding="utf-8")
    try:
        yield
    finally:
        # Only remove OUR lock: a takeover race must not delete the winner's.
        try:
            if RUN_LOCK.exists() and RUN_LOCK.read_text(encoding="utf-8").strip().split()[0] == str(os.getpid()):
                RUN_LOCK.unlink()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--static", action="store_true", help="historical 4-gate governance check (CI contract)")
    ap.add_argument("--quick", action="store_true", help="tiers 0-1 only")
    ap.add_argument(
        "--status-md",
        metavar="PATH",
        help="also write the generated gate-status file (docs/STATUS.md) — the single "
        "source of truth docs link to instead of restating gate state by hand",
    )
    args = ap.parse_args()
    if args.static:
        # The static governance check runs no legs, opens no ports and writes no STATUS.md, so it is
        # not what the lock protects against and must stay runnable alongside a sweep.
        static_main()
    else:
        with single_instance():
            aggregate_main(quick=args.quick, status_md=args.status_md)


if __name__ == "__main__":
    main()
