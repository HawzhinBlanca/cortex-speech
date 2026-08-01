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
    if os.environ.get("CORTEX_AUDIO"):
        return None
    if not (SRC_TAURI / "tests" / "fixtures" / "fleurs_ckb_sample.wav").exists():
        return "committed audio fixture missing - set CORTEX_AUDIO=<absolute wav path> instead"
    return None


# Every place a model can legitimately live, mirroring models.rs::model_root_candidates.
_MODEL_ROOTS = [SRC_TAURI / "models", SRC_TAURI / "target" / "release" / "models"]

def _probe_ipc_harness(extra=None):
    """Shared probe for the IPC e2e harnesses: the exe, the committed fixture, and any extra model.

    They now default to that fixture and run against a DISPOSABLE profile (e2e_profile.cjs), so the
    only honest reasons left to skip are a missing binary or a missing model — not a forgotten env var.
    """
    if not EXE.exists():
        return "release exe missing - run `make build-app`"
    if not (SRC_TAURI / "tests" / "fixtures" / "fleurs_ckb_sample.wav").exists():
        return "committed audio fixture missing"
    if extra is not None and not any((root / extra).exists() for root in _MODEL_ROOTS):
        return f"{extra} not present in any model root"
    return None


def _probe_silero():
    if (SRC_TAURI / "models" / "silero_vad_v4.onnx").exists():
        return None
    return "models missing - run `cd cortex-speech-app && npm run fetch-models`"


def _probe_rtf():
    if (SRC_TAURI / "models" / "omniasr-ctc-300m" / "model.int8.onnx").exists():
        return None
    return "OmniASR CTC-300M model missing - run fetch-models"


def _probe_egress():
    if not EXE.exists():
        return "release exe missing - run `make build-app`"
    if sys.platform != "win32":
        return "egress probe samples Windows TCP (Get-NetTCPConnection); runs on the owner Windows rig"
    # Require the CTC model the transcribe leg needs — the SAME location the app resolves it from and
    # every other model gate checks (src-tauri/models). Without this the gate would run browse-only yet
    # its charter advertises ASR-path coverage: SKIP-ENV honestly (INCOMPLETE) instead of a false GREEN.
    if not (SRC_TAURI / "models" / "omniasr-ctc-300m" / "model.int8.onnx").exists():
        return "OmniASR CTC-300M model missing (transcribe leg requires it) - run fetch-models"
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
    ("python-policies", 1, "cmd", "npm run test:python-policies", APP, None, "honesty/privacy/CI/dataset policy tests"),
    ("typecheck", 1, "cmd", "npm run typecheck", APP, None, "svelte-check + tsc"),
    ("lint-js", 1, "cmd", "npm run lint", APP, None, "eslint"),
    ("clippy", 1, "cmd", f'cargo clippy --manifest-path "{MANIFEST}" --all-targets -- -D warnings', REPO_ROOT, None, "Engineering rigor: clippy -D warnings"),
    ("fmt-check", 1, "cmd", f'cargo fmt --manifest-path "{MANIFEST}" --all -- --check', REPO_ROOT, None, "rustfmt"),
    ("test-frontend", 1, "cmd", "npm test", APP, None, "vitest"),
    ("test-rust", 1, "cmd", f'cargo test --manifest-path "{MANIFEST}" --jobs 4', REPO_ROOT, None, "Sorani goldens, wer-vs-jiwer, holdout hash, ONNX manifest, proof-metadata"),
    ("audit", 1, "cmd", "npm audit --omit=dev", APP, None, "npm supply chain"),
    ("deny", 1, "cmd", f'cargo deny --manifest-path "{MANIFEST}" check', REPO_ROOT, _probe_deny, "cargo supply chain"),
    ("test-e2e+a11y", 1, "cmd", "npm run test:e2e", APP, None, "A11y: axe WCAG 2.2 AA en+ckb/RTL (coverage assertion: WS2 follow-up)"),
    # Tier 2 — real binary on this machine (the personal-use core)
    ("exe-freshness", 2, "cmd", f'"{sys.executable}" "{APP / "scripts" / "check_exe_freshness.py"}"', REPO_ROOT, _probe_exe, "Truth-in-advertising: exe compiled from HEAD"),
    ("real-app-e2e", 2, "cmd", f'node "{APP / "e2e_real_app.cjs"}"', APP, _probe_real_e2e, "THE daily-use reliability gate: real exe, real audio, real transcript"),
    # Tier 3 — deep proof legs (env-gated; skipped honestly when absent)
    ("egress-runtime", 3, "cmd", f'node "{APP / "scripts" / "egress_probe.cjs"}"', APP, _probe_egress, "Privacy: zero outbound sockets at runtime — egress_probe.cjs proves ZERO external TCP from the backend PID across startup + browse + a REAL offline transcription (import->VAD->CTC ASR, the path where cloud STT/LLM would fire if consent leaked), with an in-run POSITIVE CONTROL that fails loud if the sampler is dead (no vacuous pass). Poll-sampled (200ms) + TCP-only; an airtight kernel/ETW socket trace is a further stretch. SKIP-ENV off the Windows rig / without the exe. Static test_cloud_privacy_policy.py is belt-and-braces."),
    ("ignored-real-model", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --jobs 4 -- --ignored --skip live_transcribe_segments --skip refinery_lift', REPO_ROOT, _probe_silero, "WS3a: the 37 #[ignore] real-model gates (cloud-key test excluded; refinery_lift benchmark+diag excluded — the benchmark has its own dedicated leg)"),
    # Deliberately count-agnostic: the gate enumerates targets with `cargo fuzz list` and fails loud on an
    # empty list, so hardcoding a number here only creates a second place to go stale. It said "5" until
    # the `features` target was removed with the dead FbankExtractor module it fuzzed (iteration 231).
    ("fuzz-smoke", 3, "fn", _fn_fuzz_smoke, None, _probe_fuzz, "Engineering rigor: every fuzz target, 0 crashes"),
    ("rtf-bench", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --test real_audio -- --ignored omniasr_rtf_on_committed_fleurs_ckb_fixture --nocapture', REPO_ROOT, _probe_rtf, "Latency: RTF on this rig (baseline-regression gate: WS4)"),
    ("refinery-lift", 3, "cmd", f'cargo test --manifest-path "{MANIFEST}" --test refinery_lift -- --ignored refinery_lift_injected_error_benchmark --nocapture', REPO_ROOT, None, "Refinery: >=30% CER reduction at <=15% escalation (fixed-seed injected-error benchmark, offline T0 path)"),
    ("fairness-gender-age", 3, "cmd", f'"{sys.executable}" "{APP / "scripts" / "fairness_gate.py"}"', REPO_ROOT, None, "WS4: gender/age CER disparity budget on committed corpus metadata (CORDI dialect leg owner-gated)"),
    ("pipeline-ipc-e2e", 3, "cmd", f'node "{APP / "e2e_pipeline_ipc.cjs"}"', APP, _probe_ipc_harness, "Import->VAD->ASR over the REAL IPC on a disposable profile, independent of webview rendering"),
    ("constrained-ipc-e2e", 3, "cmd", f'node "{APP / "e2e_constrained_ipc.cjs"}"', APP, _probe_ipc_harness, "transcribe_segment_constrained on the real exe: non-blank Kurdish, no Latin leak"),
    ("finetuned-ipc-e2e", 3, "cmd", f'node "{APP / "e2e_finetuned_ipc.cjs"}"', APP, lambda: _probe_ipc_harness("finetuned-mms-ckb"), "transcribe_segment_finetuned resolves + loads + decodes the embedded champion model"),
]

# Charter DoD legs descoped by the owner amendment (2026-07-10) — always printed.
DESCOPED = [
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
    ("branch-protection", "item 49: repo-admin clicks"),
    ("asosoft-600-licensing", "WS1 tail: eval-use license verification"),
]


LOG_DIR = Path(tempfile.gettempdir()) / "cortex-verify10"


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
    if r.returncode == 0:
        return PASS, secs, retried.strip()
    tail = "\n".join(((r.stdout or "") + "\n" + (r.stderr or "")).strip().splitlines()[-12:])
    return FAIL, secs, f"exit {r.returncode}{retried} - full log: {log_path}\n{tail}"


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
    print("=" * 68)
    print(" CORTEX VERIFY-10 - PERSONAL-USE FULL-CHARTER GATE")
    print(f" repo: {REPO_ROOT}   HEAD: {head}   mode: {'quick (tiers 0-1)' if quick else 'full'}")
    print(f" per-gate logs: {LOG_DIR}")
    print("=" * 68)

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
    if status_md:
        write_status_md(status_md, head, quick, results, verdict)
    sys.exit(code)


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
        static_main()
    else:
        aggregate_main(quick=args.quick, status_md=args.status_md)


if __name__ == "__main__":
    main()
