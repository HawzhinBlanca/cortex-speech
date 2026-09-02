#!/usr/bin/env python3
"""Run and validate the exact owner-critical Rust and renderer mutation campaigns.

Certifying mode requires a clean checkout and executes in a detached disposable Git worktree.  The
source checkout is never mutated: cargo-mutants may use ``--in-place`` only inside that disposable
copy (needed because Tauri's ``frontendDist`` lives above the Rust package).  Official cargo-mutants
and Stryker reports, logs, configs, and event streams are preserved; the summary is a projection,
never the evidence authority.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path, PurePosixPath
from typing import Any, Iterable


APP = Path(__file__).resolve().parents[1]
REPO = APP.parent
TAURI = APP / "src-tauri"
CONTRACT_PATH = Path(__file__).with_name("owner_mutation_contract.v1.json")
STRYKER_CONFIG = APP / "stryker.owner-critical.config.json"
RUST_QUALITY = Path(__file__).with_name("rust_quality_gate.py")
FRONTEND_REPORT = Path("reports/owner-mutation/frontend/stryker-report.json")
FRONTEND_EVENTS = Path("reports/owner-mutation/frontend/events")
FRONTEND_LOG = Path("stryker.log")
SHA256 = re.compile(r"[0-9a-f]{64}")
RAW_MANIFEST_NAME = "owner-mutation-raw-manifest.json"
RAW_BUNDLE_NAME = "owner-mutation-raw.v1.bin"
RAW_BUNDLE_FORMAT = "CORTEX_OWNER_MUTATION_RAW_V1"
RAW_BUNDLE_MAGIC = f"{RAW_BUNDLE_FORMAT}\n".encode("ascii")
MAX_BUNDLE_HEADER_BYTES = 1024 * 1024
AUTHORITY_PATHS = {
    "contract": "scripts/owner_mutation_contract.v1.json",
    "runner": "scripts/run_owner_mutation_campaign.py",
    "rustCriticalDomains": "scripts/rust_quality_gate.py",
    "strykerConfig": "stryker.owner-critical.config.json",
    "vitestMutationConfig": "vitest.mutation.config.ts",
    "package": "package.json",
    "packageLock": "package-lock.json",
    "cargoManifest": "src-tauri/Cargo.toml",
    "cargoLock": "src-tauri/Cargo.lock",
}
NPM = "npm.cmd" if os.name == "nt" else "npm"


class CampaignError(RuntimeError):
    """The campaign is incomplete, stale, malformed, or below a locked floor."""


def _strict_json(path: Path) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in rows:
            if key in value:
                raise CampaignError(f"{path} contains duplicate JSON key {key!r}")
            value[key] = item
        return value

    try:
        return json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=pairs,
            parse_constant=lambda token: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON number {token}")
            ),
        )
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        raise CampaignError(f"cannot read strict JSON {path}: {error}") from error


def _strict_json_bytes(value: bytes, *, label: str) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, item in rows:
            if key in result:
                raise ValueError(f"duplicate JSON key {key!r}")
            result[key] = item
        return result

    try:
        return json.loads(
            value.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=lambda token: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON number {token}")
            ),
        )
    except (UnicodeError, json.JSONDecodeError, ValueError) as error:
        raise CampaignError(f"cannot read strict JSON {label}: {error}") from error


def _canonical_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        with temporary.open("xb") as target:
            target.write(_canonical_json(value))
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _run_text(args: list[str], *, cwd: Path) -> str:
    completed = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        shell=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if completed.returncode != 0:
        raise CampaignError(
            f"command failed ({completed.returncode}): {args!r}\n{completed.stdout}\n{completed.stderr}"
        )
    return completed.stdout.strip()


def _run_logged(
    args: list[str],
    *,
    cwd: Path,
    log_path: Path,
    logical_log_path: str,
    allowed_return_codes: tuple[int, ...] = (0,),
    environment: dict[str, str] | None = None,
) -> dict[str, Any]:
    if not allowed_return_codes:
        raise CampaignError("a logged command must declare at least one allowed return code")
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = _utc_now()
    with log_path.open("xb") as log:
        header = _canonical_json(
            {
                "argv": args,
                "cwd": str(cwd),
                "environmentOverrides": environment or {},
                "startedAt": started,
            }
        )
        log.write(header)
        log.flush()
        os.fsync(log.fileno())
        try:
            completed = subprocess.run(
                args,
                cwd=cwd,
                env={**os.environ, **(environment or {})},
                check=False,
                shell=False,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            return_code = completed.returncode
        except OSError as error:
            log.write(_canonical_json({"spawnError": str(error)}))
            return_code = None
        ended = _utc_now()
        log.write(_canonical_json({"endedAt": ended, "returnCode": return_code}))
        log.flush()
        os.fsync(log.fileno())
    if return_code is None:
        raise CampaignError(f"could not start command: {args!r}")
    record = {
        "argv": args,
        "cwd": str(cwd),
        "environmentOverrides": environment or {},
        "allowedReturnCodes": list(allowed_return_codes),
        "startedAt": started,
        "endedAt": ended,
        "returnCode": return_code,
        "logPath": logical_log_path,
        "logSha256": _sha256_file(log_path),
        "logBytes": log_path.stat().st_size,
    }
    if return_code not in allowed_return_codes:
        raise CampaignError(
            f"command returned {return_code}, expected one of {allowed_return_codes}: {args!r}"
        )
    return record


def _run_split_logged(
    args: list[str],
    *,
    cwd: Path,
    stdout_path: Path,
    logical_stdout_path: str,
    stderr_path: Path,
    logical_stderr_path: str,
) -> dict[str, Any]:
    """Run a JSON-producing native command while retaining both byte streams durably."""

    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    started = _utc_now()
    with stdout_path.open("xb") as stdout, stderr_path.open("xb") as stderr:
        completed = subprocess.run(
            args,
            cwd=cwd,
            check=False,
            shell=False,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
        )
        stdout.flush()
        os.fsync(stdout.fileno())
        stderr.flush()
        os.fsync(stderr.fileno())
    ended = _utc_now()
    record = {
        "argv": args,
        "cwd": str(cwd),
        "environmentOverrides": {},
        "allowedReturnCodes": [0],
        "startedAt": started,
        "endedAt": ended,
        "returnCode": completed.returncode,
        "stdoutPath": logical_stdout_path,
        "stdoutSha256": _sha256_file(stdout_path),
        "stdoutBytes": stdout_path.stat().st_size,
        "logPath": logical_stderr_path,
        "logSha256": _sha256_file(stderr_path),
        "logBytes": stderr_path.stat().st_size,
    }
    if completed.returncode != 0:
        raise CampaignError(f"command returned {completed.returncode}, expected 0: {args!r}")
    return record


def _utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _load_contract() -> dict[str, Any]:
    value = _strict_json(CONTRACT_PATH)
    thresholds = value.get("thresholds") if isinstance(value, dict) else None
    if (
        not isinstance(value, dict)
        or set(value) != {"schema", "type", "thresholds", "backend", "frontend"}
        or type(value.get("schema")) is not int
        or value["schema"] != 1
        or value.get("type") != "OwnerMutationCampaignContractV1"
        or not isinstance(thresholds, dict)
        or set(thresholds) != {"backendCriticalDomainPercent", "frontendReducerDomainPercent"}
        or type(thresholds.get("backendCriticalDomainPercent")) is not int
        or thresholds["backendCriticalDomainPercent"] != 90
        or type(thresholds.get("frontendReducerDomainPercent")) is not int
        or thresholds["frontendReducerDomainPercent"] != 80
    ):
        raise CampaignError("owner mutation contract has a non-canonical envelope or relaxed floor")
    backend = value.get("backend")
    frontend = value.get("frontend")
    if (
        not isinstance(backend, dict)
        or backend.get("version") != "27.1.0"
        or backend.get("allowedExitCodes") != [0, 2, 3]
        or any(type(code) is not int for code in backend.get("allowedExitCodes", []))
        or type(backend.get("jobs")) is not int
        or backend["jobs"] != 2
        or type(backend.get("minimumTestTimeoutSeconds")) is not int
        or backend["minimumTestTimeoutSeconds"] != 120
        or type(backend.get("timeoutMultiplier")) is not float
        or backend["timeoutMultiplier"] != 4.0
        or type(backend.get("buildTimeoutMultiplier")) is not float
        or backend["buildTimeoutMultiplier"] != 4.0
    ):
        raise CampaignError("backend mutation tool is not pinned to cargo-mutants 27.1.0")
    if (
        not isinstance(frontend, dict)
        or frontend.get("coreVersion") != "10.0.0"
        or frontend.get("vitestRunnerVersion") != "10.0.0"
        or frontend.get("vitestVersion") != "4.1.10"
        or frontend.get("allowedExitCodes") != [0]
        or any(type(code) is not int for code in frontend.get("allowedExitCodes", []))
        or "Ignored" in frontend.get("allowedRawStatuses", [])
    ):
        raise CampaignError("frontend mutation toolchain is not exactly pinned")
    return value


def _rust_domains(app: Path = APP) -> dict[str, tuple[str, ...]]:
    authority = app / "scripts" / RUST_QUALITY.name
    spec = importlib.util.spec_from_file_location("cortex_rust_quality_gate", authority)
    if spec is None or spec.loader is None:
        raise CampaignError("cannot import the Rust critical-domain authority")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    domains = getattr(module, "CRITICAL_COVERAGE_DOMAINS", None)
    if not isinstance(domains, dict) or not domains:
        raise CampaignError("Rust critical-domain authority is empty")
    return {str(name): tuple(str(pattern) for pattern in patterns) for name, patterns in domains.items()}


def _expanded_backend_sources(app: Path) -> tuple[list[str], dict[str, list[str]]]:
    domains = _rust_domains(app)
    available = sorted(
        path.relative_to(app).as_posix()
        for path in (app / "src-tauri" / "src").rglob("*.rs")
        if path.is_file()
    )
    by_domain: dict[str, list[str]] = {}
    for domain, patterns in domains.items():
        matched: set[str] = set()
        for pattern in patterns:
            rows = {path for path in available if fnmatch.fnmatchcase(path, pattern)}
            if not rows:
                raise CampaignError(f"critical backend pattern matches no source: {domain}/{pattern}")
            matched.update(rows)
        by_domain[domain] = sorted(matched)
    return sorted({path for rows in by_domain.values() for path in rows}), by_domain


def _tool_versions(app: Path, contract: dict[str, Any]) -> dict[str, str]:
    cargo = _run_text(["cargo", "mutants", "--version"], cwd=app / "src-tauri")
    expected_cargo = f"cargo-mutants {contract['backend']['version']}"
    if cargo != expected_cargo:
        raise CampaignError(f"cargo-mutants identity mismatch: {cargo!r} != {expected_cargo!r}")
    package_lock = _strict_json(app / "package-lock.json")
    packages = package_lock.get("packages") if isinstance(package_lock, dict) else None
    expected_packages = {
        "node_modules/@stryker-mutator/core": contract["frontend"]["coreVersion"],
        "node_modules/@stryker-mutator/vitest-runner": contract["frontend"]["vitestRunnerVersion"],
        "node_modules/vitest": contract["frontend"]["vitestVersion"],
    }
    if not isinstance(packages, dict) or any(
        not isinstance(packages.get(path), dict)
        or packages[path].get("version") != version
        for path, version in expected_packages.items()
    ):
        raise CampaignError("package-lock does not pin the exact frontend mutation toolchain")
    return {
        "cargoMutants": cargo.removeprefix("cargo-mutants "),
        "stryker": str(contract["frontend"]["coreVersion"]),
        "strykerVitestRunner": str(contract["frontend"]["vitestRunnerVersion"]),
        "vitest": str(contract["frontend"]["vitestVersion"]),
    }


def _backend_list_command(app: Path, sources: Iterable[str]) -> list[str]:
    command = ["cargo", "mutants", "--list", "--json", "--no-config"]
    for source in sources:
        command.extend(("--file", str(PurePosixPath(source).relative_to("src-tauri"))))
    return command


def _backend_run_command(
    app: Path,
    sources: Iterable[str],
    contract: dict[str, Any],
    output: Path,
) -> list[str]:
    backend = contract["backend"]
    command = [
        "cargo",
        "mutants",
        "--in-place",
        "--no-config",
        "--no-shuffle",
        "--all-features",
        "--jobs",
        str(backend["jobs"]),
        "--minimum-test-timeout",
        str(backend["minimumTestTimeoutSeconds"]),
        "--timeout-multiplier",
        str(backend["timeoutMultiplier"]),
        "--build-timeout-multiplier",
        str(backend["buildTimeoutMultiplier"]),
        "--output",
        str(output),
        "--cargo-arg",
        "--lib",
    ]
    for source in sources:
        command.extend(("--file", str(PurePosixPath(source).relative_to("src-tauri"))))
    return command


def _mutant_identity(mutant: dict[str, Any]) -> str:
    identity = {
        "file": mutant.get("file"),
        "function": mutant.get("function"),
        "genre": mutant.get("genre"),
        "name": mutant.get("name"),
        "package": mutant.get("package"),
        "replacement": mutant.get("replacement"),
        "span": mutant.get("span"),
    }
    return _sha256_bytes(_canonical_json(identity))


def _validate_backend_raw(
    *,
    inventory_path: Path,
    native_inventory_path: Path,
    outcomes_path: Path,
    app: Path,
    by_domain: dict[str, list[str]],
    minimum: float,
) -> dict[str, Any]:
    inventory = _strict_json(inventory_path)
    native_inventory = _strict_json(native_inventory_path)
    outcomes = _strict_json(outcomes_path)
    if not isinstance(inventory, list) or not inventory:
        raise CampaignError("cargo-mutants inventory is empty")
    if _canonical_json(native_inventory) != _canonical_json(inventory):
        raise CampaignError("cargo-mutants native mutant inventory differs from the pre-run inventory")
    if not isinstance(outcomes, dict) or outcomes.get("cargo_mutants_version") != "27.1.0":
        raise CampaignError("cargo-mutants outcomes have the wrong producer identity")
    rows = outcomes.get("outcomes")
    baseline = rows[0] if isinstance(rows, list) and rows else None
    if (
        not isinstance(baseline, dict)
        or baseline.get("scenario") != "Baseline"
        or baseline.get("summary") != "Success"
    ):
        raise CampaignError("cargo-mutants has no successful unmutated baseline")
    allowed_sources = {
        str(PurePosixPath(path).relative_to("src-tauri"))
        for domain_sources in by_domain.values()
        for path in domain_sources
    }
    expected: dict[str, dict[str, Any]] = {}
    for mutant in inventory:
        if not isinstance(mutant, dict):
            raise CampaignError("cargo-mutants inventory row is malformed")
        identifier = _mutant_identity(mutant)
        if identifier in expected:
            raise CampaignError("cargo-mutants inventory duplicates a mutant identity")
        source = mutant.get("file")
        source_path = PurePosixPath(source) if isinstance(source, str) else None
        if (
            not isinstance(source, str)
            or "\\" in source
            or source_path is None
            or source_path.is_absolute()
            or any(part in {"", ".", ".."} for part in source_path.parts)
            or source not in allowed_sources
            or not (app / "src-tauri" / source_path).is_file()
        ):
            raise CampaignError("cargo-mutants inventory references an absent source")
        expected[identifier] = mutant
    observed: dict[str, str] = {}
    raw_to_normalized = {
        "CaughtMutant": "KILLED",
        "MissedMutant": "SURVIVED",
        "Timeout": "TIMEOUT",
        "Unviable": "BUILD_ERROR",
    }
    for row in rows[1:]:
        scenario = row.get("scenario") if isinstance(row, dict) else None
        mutant = scenario.get("Mutant") if isinstance(scenario, dict) else None
        summary = row.get("summary") if isinstance(row, dict) else None
        if not isinstance(mutant, dict) or summary not in raw_to_normalized:
            raise CampaignError("cargo-mutants outcome is malformed or unsupported")
        identifier = _mutant_identity(mutant)
        if identifier not in expected or identifier in observed:
            raise CampaignError("cargo-mutants outcome is unknown or duplicated")
        observed[identifier] = raw_to_normalized[summary]
    if set(observed) != set(expected):
        raise CampaignError("cargo-mutants did not execute every discovered mutant exactly once")
    domains: dict[str, Any] = {}
    for domain, app_sources in by_domain.items():
        package_sources = {str(PurePosixPath(path).relative_to("src-tauri")) for path in app_sources}
        domain_rows = [
            observed[identifier]
            for identifier, mutant in expected.items()
            if mutant["file"] in package_sources
        ]
        if not domain_rows:
            raise CampaignError(f"cargo-mutants discovered no mutants in mandatory domain {domain}")
        killed = domain_rows.count("KILLED")
        score = killed * 100.0 / len(domain_rows)
        domains[domain] = {"mutants": len(domain_rows), "killed": killed, "scorePercent": score}
        if score + 1e-12 < minimum:
            raise CampaignError(f"backend mutation domain {domain} is below {minimum:g}% ({score:.2f}%)")
    return {"mutants": len(expected), "killed": list(observed.values()).count("KILLED"), "domains": domains}


def _validate_frontend_raw(
    app: Path,
    contract: dict[str, Any],
    minimum: float,
    *,
    report_path: Path | None = None,
    event_directory: Path | None = None,
    log_path: Path | None = None,
) -> dict[str, Any]:
    report_path = report_path or app / FRONTEND_REPORT
    report = _strict_json(report_path)
    if (
        not isinstance(report, dict)
        or report.get("schemaVersion") != "1.0"
        or not isinstance(report.get("files"), dict)
        or not report["files"]
    ):
        raise CampaignError("Stryker report is empty or has the wrong schema")
    framework = report.get("framework")
    thresholds = report.get("thresholds")
    if (
        not isinstance(framework, dict)
        or framework.get("name") != "StrykerJS"
        or framework.get("version") != contract["frontend"]["coreVersion"]
        or not isinstance(thresholds, dict)
        or set(thresholds) != {"high", "low", "break"}
        or type(thresholds.get("high")) is not int
        or thresholds["high"] != 100
        or type(thresholds.get("low")) is not int
        or thresholds["low"] != minimum
        or type(thresholds.get("break")) is not int
        or thresholds["break"] != minimum
    ):
        raise CampaignError("Stryker report does not prove the pinned producer and exact floor")
    domains = contract["frontend"]["domains"]
    expected_sources = {
        source: domain
        for domain, value in domains.items()
        for source in value["sources"]
    }
    if set(report["files"]) != set(expected_sources):
        raise CampaignError("Stryker mutated a substituted or incomplete source inventory")
    allowed = set(contract["frontend"]["allowedRawStatuses"])
    by_domain: dict[str, list[str]] = {domain: [] for domain in domains}
    mutant_ids: set[str] = set()
    for source, file_row in report["files"].items():
        source_path = PurePosixPath(source) if isinstance(source, str) else None
        actual_source = app / source_path if source_path is not None else None
        if (
            not isinstance(source, str)
            or "\\" in source
            or source_path is None
            or source_path.is_absolute()
            or any(part in {"", ".", ".."} for part in source_path.parts)
            or actual_source is None
            or not actual_source.is_file()
            or not isinstance(file_row, dict)
            or file_row.get("source") != actual_source.read_bytes().decode("utf-8")
            or not isinstance(file_row.get("mutants"), list)
            or not file_row["mutants"]
        ):
            raise CampaignError(f"Stryker source {source} has no measured mutants")
        for mutant in file_row["mutants"]:
            identifier = mutant.get("id") if isinstance(mutant, dict) else None
            status = mutant.get("status") if isinstance(mutant, dict) else None
            if not isinstance(identifier, str) or identifier in mutant_ids or status not in allowed:
                raise CampaignError("Stryker mutant identity/status is invalid or duplicated")
            mutant_ids.add(identifier)
            by_domain[expected_sources[source]].append(str(status))
    results: dict[str, Any] = {}
    for domain, statuses in by_domain.items():
        killed = statuses.count("Killed")
        score = killed * 100.0 / len(statuses)
        results[domain] = {"mutants": len(statuses), "killed": killed, "scorePercent": score}
        if score + 1e-12 < minimum:
            raise CampaignError(f"frontend mutation domain {domain} is below {minimum:g}% ({score:.2f}%)")
    event_directory = event_directory or app / FRONTEND_EVENTS
    events = sorted(event_directory.glob("*.json"))
    tested = [path for path in events if path.name.endswith("-onMutantTested.json")]
    dry = [path for path in events if path.name.endswith("-onDryRunCompleted.json")]
    plans = [path for path in events if path.name.endswith("-onMutationTestingPlanReady.json")]
    ready = [path for path in events if path.name.endswith("-onMutationTestReportReady.json")]
    if (
        len(tested) != len(mutant_ids)
        or len(dry) != 1
        or len(plans) != 1
        or len(ready) != 1
        or len(events) != len(tested) + len(dry) + len(plans) + len(ready)
    ):
        raise CampaignError("Stryker event stream does not contain one raw outcome per discovered mutant")
    expected_event_prefixes = list(range(len(events)))
    observed_event_prefixes = []
    for event in events:
        match = re.fullmatch(r"(\d{5})-[A-Za-z]+\.json", event.name)
        if match is None:
            raise CampaignError("Stryker event stream contains a non-canonical event filename")
        observed_event_prefixes.append(int(match.group(1)))
    if (
        observed_event_prefixes != expected_event_prefixes
        or events[0] != dry[0]
        or events[1] != plans[0]
        or events[-1] != ready[0]
    ):
        raise CampaignError("Stryker event stream is reordered, truncated, or non-contiguous")
    dry_value = _strict_json(dry[0])
    dry_result = dry_value.get("result") if isinstance(dry_value, dict) else None
    expected_test_files = {
        test
        for domain in contract["frontend"]["domains"].values()
        for test in domain["tests"]
    }
    if (
        not isinstance(dry_result, dict)
        or dry_result.get("status") != "complete"
        or not isinstance(dry_result.get("tests"), list)
        or not dry_result["tests"]
    ):
        raise CampaignError("Stryker dry-run event does not prove one clean passing baseline")
    observed_test_ids: set[str] = set()
    observed_test_files: set[str] = set()
    resolved_app = app.resolve()
    for test in dry_result["tests"]:
        identifier = test.get("id") if isinstance(test, dict) else None
        filename = test.get("fileName") if isinstance(test, dict) else None
        try:
            relative_test = Path(filename).resolve().relative_to(resolved_app).as_posix()
        except (OSError, TypeError, ValueError):
            relative_test = ""
        if (
            not isinstance(test, dict)
            or type(test.get("status")) is not int
            or test["status"] != 0
            or not isinstance(identifier, str)
            or not identifier
            or identifier in observed_test_ids
            or not isinstance(filename, str)
            or relative_test not in expected_test_files
            or not identifier.startswith(f"{relative_test}#")
        ):
            raise CampaignError("Stryker dry-run test identity/status is incomplete or substituted")
        observed_test_ids.add(identifier)
        observed_test_files.add(relative_test)
    if observed_test_files != expected_test_files:
        raise CampaignError("Stryker dry run did not execute every exact declared test file")
    plan_value = _strict_json(plans[0])
    plan_rows = plan_value.get("mutantPlans") if isinstance(plan_value, dict) else None
    plan_ids = {
        row.get("mutant", {}).get("id")
        for row in plan_rows
        if isinstance(row, dict) and isinstance(row.get("mutant"), dict)
    } if isinstance(plan_rows, list) else set()
    if len(plan_rows or []) != len(mutant_ids) or plan_ids != mutant_ids:
        raise CampaignError("Stryker execution plan does not enumerate the exact discovered mutants")
    raw_statuses: dict[str, str] = {}
    for path in tested:
        row = _strict_json(path)
        identifier = row.get("id") if isinstance(row, dict) else None
        status = row.get("status") if isinstance(row, dict) else None
        if identifier not in mutant_ids or identifier in raw_statuses or status not in allowed:
            raise CampaignError("Stryker raw outcome event is unknown, duplicated, or malformed")
        raw_statuses[str(identifier)] = str(status)
    report_statuses = {
        str(mutant["id"]): str(mutant["status"])
        for file_row in report["files"].values()
        for mutant in file_row["mutants"]
    }
    if raw_statuses != report_statuses or _canonical_json(_strict_json(ready[0])) != _canonical_json(report):
        raise CampaignError("Stryker JSON report is not an exact projection of its raw event stream")
    if not (log_path or app / FRONTEND_LOG).is_file():
        raise CampaignError("Stryker trace log is absent")
    return {"mutants": len(mutant_ids), "killed": sum(row["killed"] for row in results.values()), "domains": results}


def _artifact_inventory(root: Path) -> list[dict[str, Any]]:
    return [
        {
            "path": path.relative_to(root).as_posix(),
            "sha256": _sha256_file(path),
            "bytes": path.stat().st_size,
        }
        for path in sorted(root.rglob("*"))
        if path.is_file()
    ]


def _safe_bundle_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if (
        not value
        or "\\" in value
        or path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
    ):
        raise CampaignError(f"unsafe mutation bundle path: {value!r}")
    return path


def _is_linklike(path: Path) -> bool:
    is_junction = getattr(path, "is_junction", None)
    return path.is_symlink() or bool(is_junction and is_junction())


def _regular_files(root: Path) -> list[Path]:
    if not root.is_dir() or _is_linklike(root):
        raise CampaignError(f"mutation evidence root is absent or link-like: {root}")
    result: list[Path] = []
    for current, directories, files in os.walk(root, followlinks=False):
        current_path = Path(current)
        for directory in directories:
            child = current_path / directory
            if _is_linklike(child):
                raise CampaignError(f"mutation evidence contains a link-like directory: {child}")
        for filename in files:
            child = current_path / filename
            if _is_linklike(child) or not child.is_file():
                raise CampaignError(f"mutation evidence contains a non-regular file: {child}")
            result.append(child)
    return sorted(result, key=lambda path: path.relative_to(root).as_posix())


def _authority_inventory(app: Path) -> dict[str, dict[str, Any]]:
    authorities: dict[str, dict[str, Any]] = {}
    for name, relative in AUTHORITY_PATHS.items():
        path = app / PurePosixPath(relative)
        if _is_linklike(path) or not path.is_file():
            raise CampaignError(f"mutation authority is absent or link-like: {relative}")
        authorities[name] = {
            "path": relative,
            "bundlePath": f"authorities/{relative}",
            "sha256": _sha256_file(path),
            "bytes": path.stat().st_size,
        }
    return authorities


def _write_raw_bundle(
    bundle_path: Path,
    sources: Iterable[tuple[str, Path]],
) -> dict[str, Any]:
    rows = sorted(sources, key=lambda row: row[0])
    logical_paths = [logical for logical, _path in rows]
    if len(logical_paths) != len(set(logical_paths)):
        raise CampaignError("mutation raw bundle source inventory contains duplicate paths")
    for logical, path in rows:
        _safe_bundle_path(logical)
        if _is_linklike(path) or not path.is_file():
            raise CampaignError(f"mutation raw bundle source is absent or link-like: {path}")
    bundle_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = bundle_path.with_name(f".{bundle_path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    entries: list[dict[str, Any]] = []
    try:
        with temporary.open("xb") as bundle:
            bundle.write(RAW_BUNDLE_MAGIC)
            for logical, source in rows:
                expected_bytes = source.stat().st_size
                expected_sha = _sha256_file(source)
                header = {"bytes": expected_bytes, "path": logical, "sha256": expected_sha}
                header_bytes = _canonical_json(header)
                if len(header_bytes) > MAX_BUNDLE_HEADER_BYTES:
                    raise CampaignError("mutation raw bundle entry header is unreasonably large")
                bundle.write(struct.pack(">I", len(header_bytes)))
                bundle.write(header_bytes)
                bundle.write(struct.pack(">Q", expected_bytes))
                copied_bytes = 0
                copied_hash = hashlib.sha256()
                with source.open("rb") as stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        bundle.write(chunk)
                        copied_bytes += len(chunk)
                        copied_hash.update(chunk)
                if copied_bytes != expected_bytes or copied_hash.hexdigest() != expected_sha:
                    raise CampaignError(f"mutation raw source changed while bundling: {source}")
                entries.append(header)
            bundle.flush()
            os.fsync(bundle.fileno())
        os.replace(temporary, bundle_path)
    finally:
        temporary.unlink(missing_ok=True)
    return {
        "format": RAW_BUNDLE_FORMAT,
        "sha256": _sha256_file(bundle_path),
        "bytes": bundle_path.stat().st_size,
        "entries": entries,
    }


def _extract_raw_bundle(manifest: dict[str, Any], bundle_path: Path, destination: Path) -> None:
    bundle = manifest.get("bundle")
    if (
        not isinstance(bundle, dict)
        or set(bundle) != {"format", "sha256", "bytes", "entries"}
        or bundle.get("format") != RAW_BUNDLE_FORMAT
        or not isinstance(bundle.get("sha256"), str)
        or not SHA256.fullmatch(bundle["sha256"])
        or type(bundle.get("bytes")) is not int
        or bundle["bytes"] <= len(RAW_BUNDLE_MAGIC)
        or not isinstance(bundle.get("entries"), list)
        or not bundle["entries"]
    ):
        raise CampaignError("mutation raw bundle identity is malformed")
    if (
        not bundle_path.is_file()
        or bundle_path.stat().st_size != bundle["bytes"]
        or _sha256_file(bundle_path) != bundle["sha256"]
    ):
        raise CampaignError("mutation raw bundle bytes do not match the manifest")
    destination.mkdir(parents=True, exist_ok=False)
    observed: list[dict[str, Any]] = []
    seen: set[str] = set()
    with bundle_path.open("rb") as source:
        if source.read(len(RAW_BUNDLE_MAGIC)) != RAW_BUNDLE_MAGIC:
            raise CampaignError("mutation raw bundle has the wrong format marker")
        while source.tell() < bundle["bytes"]:
            length_bytes = source.read(4)
            if len(length_bytes) != 4:
                raise CampaignError("mutation raw bundle truncates an entry header")
            (header_length,) = struct.unpack(">I", length_bytes)
            if header_length <= 0 or header_length > MAX_BUNDLE_HEADER_BYTES:
                raise CampaignError("mutation raw bundle has an impossible entry header length")
            header_bytes = source.read(header_length)
            content_length_bytes = source.read(8)
            if len(header_bytes) != header_length or len(content_length_bytes) != 8:
                raise CampaignError("mutation raw bundle truncates an entry header")
            header = _strict_json_bytes(header_bytes, label="mutation bundle entry header")
            if (
                not isinstance(header, dict)
                or set(header) != {"bytes", "path", "sha256"}
                or header_bytes != _canonical_json(header)
                or not isinstance(header.get("path"), str)
                or header["path"] in seen
                or type(header.get("bytes")) is not int
                or header["bytes"] < 0
                or not isinstance(header.get("sha256"), str)
                or not SHA256.fullmatch(header["sha256"])
            ):
                raise CampaignError("mutation raw bundle entry is malformed, duplicated, or non-canonical")
            logical = _safe_bundle_path(header["path"])
            (content_length,) = struct.unpack(">Q", content_length_bytes)
            if content_length != header["bytes"]:
                raise CampaignError("mutation raw bundle entry length contradicts its header")
            target = destination.joinpath(*logical.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            remaining = content_length
            digest = hashlib.sha256()
            with target.open("xb") as output:
                while remaining:
                    chunk = source.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise CampaignError("mutation raw bundle truncates entry content")
                    output.write(chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
                output.flush()
                os.fsync(output.fileno())
            if digest.hexdigest() != header["sha256"]:
                raise CampaignError(f"mutation raw bundle entry is corrupted: {header['path']}")
            seen.add(header["path"])
            observed.append(header)
        if source.read(1):
            raise CampaignError("mutation raw bundle has unparsed trailing bytes")
    if (
        _canonical_json(observed) != _canonical_json(bundle["entries"])
        or [row["path"] for row in observed] != sorted(seen)
    ):
        raise CampaignError("mutation raw manifest does not enumerate the exact sorted bundle entries")


def _publish_raw_authority(
    *,
    output: Path,
    app: Path,
    full_sha: str,
    checkout_digest: str,
    run_token: str,
    scope: list[str],
    tools: dict[str, str],
) -> None:
    campaign_path = output / "campaign.json"
    authorities = _authority_inventory(app)
    sources = [
        (f"campaign/{path.relative_to(output).as_posix()}", path)
        for path in _regular_files(output)
    ]
    sources.extend(
        (row["bundlePath"], app / PurePosixPath(row["path"]))
        for row in authorities.values()
    )
    bundle_path = output / RAW_BUNDLE_NAME
    bundle = _write_raw_bundle(bundle_path, sources)
    manifest = {
        "schema": 1,
        "type": "OwnerMutationRawAuthorityV1",
        "runToken": run_token,
        "scope": scope,
        "certificationEligible": scope == ["backend", "frontend"],
        "fullGitSha": full_sha,
        "checkoutStateDigest": checkout_digest,
        "contractSha256": _sha256_file(app / AUTHORITY_PATHS["contract"]),
        "campaignSha256": _sha256_file(campaign_path),
        "authorities": authorities,
        "tools": tools,
        "runtime": {
            "python": platform.python_version(),
            "node": _run_text(["node", "--version"], cwd=app),
            "platform": platform.system(),
            "architecture": platform.machine(),
        },
        "bundle": bundle,
    }
    _atomic_json(output / RAW_MANIFEST_NAME, manifest)


def _parse_utc(value: Any, *, label: str) -> dt.datetime:
    if not isinstance(value, str) or not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", value):
        raise CampaignError(f"{label} is not a canonical UTC timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise CampaignError(f"{label} is not a valid UTC timestamp") from error
    if parsed.tzinfo != dt.timezone.utc:
        raise CampaignError(f"{label} is not UTC")
    if parsed > dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=5):
        raise CampaignError(f"{label} is implausibly far in the future")
    return parsed


def _validate_command_records(campaign_root: Path, commands: Any) -> None:
    if not isinstance(commands, list) or not commands:
        raise CampaignError("mutation campaign has no exact command records")
    claimed_paths: set[str] = set()
    previous_end: dt.datetime | None = None
    base_keys = {
        "argv",
        "cwd",
        "environmentOverrides",
        "allowedReturnCodes",
        "startedAt",
        "endedAt",
        "returnCode",
        "logPath",
        "logSha256",
        "logBytes",
    }
    stdout_keys = {"stdoutPath", "stdoutSha256", "stdoutBytes"}
    for index, row in enumerate(commands):
        if not isinstance(row, dict) or frozenset(row) not in {
            frozenset(base_keys),
            frozenset(base_keys | stdout_keys),
        }:
            raise CampaignError(f"mutation command record {index} has a non-canonical envelope")
        argv = row.get("argv")
        allowed = row.get("allowedReturnCodes")
        environment = row.get("environmentOverrides")
        if (
            not isinstance(argv, list)
            or not argv
            or any(not isinstance(arg, str) or "\x00" in arg for arg in argv)
            or not isinstance(row.get("cwd"), str)
            or not row["cwd"]
            or not isinstance(environment, dict)
            or any(not isinstance(key, str) or not isinstance(value, str) for key, value in environment.items())
            or not isinstance(allowed, list)
            or not allowed
            or len(allowed) != len(set(allowed))
            or any(type(code) is not int for code in allowed)
            or type(row.get("returnCode")) is not int
            or row["returnCode"] not in allowed
        ):
            raise CampaignError(f"mutation command record {index} is malformed or unsuccessful")
        started = _parse_utc(row.get("startedAt"), label=f"mutation command {index} start")
        ended = _parse_utc(row.get("endedAt"), label=f"mutation command {index} end")
        if ended < started or (previous_end is not None and started < previous_end):
            raise CampaignError("mutation command chronology overlaps or runs backwards")
        previous_end = ended
        for prefix in ("log", "stdout"):
            path_key = f"{prefix}Path"
            if path_key not in row:
                continue
            logical = row[path_key]
            if not isinstance(logical, str) or logical in claimed_paths:
                raise CampaignError("mutation command evidence path is missing or duplicated")
            _safe_bundle_path(logical)
            claimed_paths.add(logical)
            path = campaign_root.joinpath(*PurePosixPath(logical).parts)
            sha_key = f"{prefix}Sha256"
            bytes_key = f"{prefix}Bytes"
            if (
                not path.is_file()
                or _is_linklike(path)
                or not isinstance(row.get(sha_key), str)
                or not SHA256.fullmatch(row[sha_key])
                or type(row.get(bytes_key)) is not int
                or row[bytes_key] < 0
                or path.stat().st_size != row[bytes_key]
                or _sha256_file(path) != row[sha_key]
            ):
                raise CampaignError(f"mutation command {index} does not bind its {prefix} bytes")


def _validate_campaign_bundle_inventory(extracted: Path, scope: list[str]) -> None:
    entries = {
        path.relative_to(extracted).as_posix()
        for path in _regular_files(extracted)
    }
    authority_entries = {f"authorities/{relative}" for relative in AUTHORITY_PATHS.values()}
    if {path for path in entries if path.startswith("authorities/")} != authority_entries:
        raise CampaignError("mutation bundle omits or substitutes a committed authority")
    campaign_entries = {
        path.removeprefix("campaign/")
        for path in entries
        if path.startswith("campaign/")
    }
    if len(campaign_entries) + len(authority_entries) != len(entries):
        raise CampaignError("mutation bundle contains an unknown top-level evidence class")
    required = {"campaign.json", "setup/git-worktree.log", "setup/npm-ci.log"}
    allowed_exact = set(required)
    allowed_prefixes: list[str] = []
    if "frontend" in scope:
        frontend = {
            "frontend/run.log",
            "frontend/raw/stryker-report.json",
            "frontend/raw/stryker.log",
        }
        required.update(frontend)
        allowed_exact.update(frontend)
        allowed_prefixes.append("frontend/raw/events/")
    if "backend" in scope:
        backend = {
            "setup/frontend-build.log",
            "backend/inventory.json",
            "backend/inventory.log",
            "backend/run.log",
            "backend/raw/mutants.json",
            "backend/raw/outcomes.json",
        }
        required.update(backend)
        allowed_exact.update(backend)
        allowed_prefixes.append("backend/raw/")
    missing = required - campaign_entries
    unexpected = {
        path
        for path in campaign_entries
        if path not in allowed_exact and not any(path.startswith(prefix) for prefix in allowed_prefixes)
    }
    if missing or unexpected:
        raise CampaignError(
            f"mutation bundle campaign inventory mismatch; missing={sorted(missing)!r}, "
            f"unexpected={sorted(unexpected)!r}"
        )


def _replay_raw_authority(output: Path) -> dict[str, Any]:
    manifest_path = output / RAW_MANIFEST_NAME
    bundle_path = output / RAW_BUNDLE_NAME
    manifest = _strict_json(manifest_path)
    expected_keys = {
        "schema",
        "type",
        "runToken",
        "scope",
        "certificationEligible",
        "fullGitSha",
        "checkoutStateDigest",
        "contractSha256",
        "campaignSha256",
        "authorities",
        "tools",
        "runtime",
        "bundle",
    }
    scope = manifest.get("scope") if isinstance(manifest, dict) else None
    if (
        not isinstance(manifest, dict)
        or set(manifest) != expected_keys
        or type(manifest.get("schema")) is not int
        or manifest["schema"] != 1
        or manifest.get("type") != "OwnerMutationRawAuthorityV1"
        or not isinstance(manifest.get("runToken"), str)
        or not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", manifest["runToken"])
        or scope not in (["frontend"], ["backend"], ["backend", "frontend"])
        or type(manifest.get("certificationEligible")) is not bool
        or manifest.get("certificationEligible") != (scope == ["backend", "frontend"])
        or not isinstance(manifest.get("fullGitSha"), str)
        or not re.fullmatch(r"[0-9a-f]{40,64}", manifest["fullGitSha"])
        or not isinstance(manifest.get("checkoutStateDigest"), str)
        or not SHA256.fullmatch(manifest["checkoutStateDigest"])
        or manifest["checkoutStateDigest"] != _sha256_bytes(b"")
        or not isinstance(manifest.get("contractSha256"), str)
        or not SHA256.fullmatch(manifest["contractSha256"])
        or not isinstance(manifest.get("campaignSha256"), str)
        or not SHA256.fullmatch(manifest["campaignSha256"])
    ):
        raise CampaignError("mutation raw authority manifest is malformed or non-canonical")
    current_sha = _run_text(["git", "rev-parse", "HEAD"], cwd=REPO)
    if current_sha != manifest["fullGitSha"]:
        raise CampaignError("mutation raw authority belongs to a different Git commit")
    current_authorities = _authority_inventory(APP)
    if _canonical_json(manifest.get("authorities")) != _canonical_json(current_authorities):
        raise CampaignError("mutation raw authority hashes do not match the current committed inputs")
    if manifest["contractSha256"] != current_authorities["contract"]["sha256"]:
        raise CampaignError("mutation contract hash is inconsistent")
    contract = _load_contract()
    tools = _tool_versions(APP, contract)
    if manifest.get("tools") != tools:
        raise CampaignError("mutation raw authority tool identities are stale or substituted")
    runtime = manifest.get("runtime")
    if (
        not isinstance(runtime, dict)
        or set(runtime) != {"python", "node", "platform", "architecture"}
        or runtime.get("python") != platform.python_version()
        or runtime.get("node") != _run_text(["node", "--version"], cwd=APP)
        or runtime.get("platform") != platform.system()
        or runtime.get("architecture") != platform.machine()
    ):
        raise CampaignError("mutation raw authority runtime identity does not match this replay host")
    with tempfile.TemporaryDirectory(prefix="cortex-mutation-replay-") as directory:
        extracted = Path(directory) / "raw"
        _extract_raw_bundle(manifest, bundle_path, extracted)
        _validate_campaign_bundle_inventory(extracted, scope)
        for row in current_authorities.values():
            bundled = extracted.joinpath(*PurePosixPath(row["bundlePath"]).parts)
            if bundled.stat().st_size != row["bytes"] or _sha256_file(bundled) != row["sha256"]:
                raise CampaignError("mutation bundle authority bytes contradict the manifest")
        campaign_root = extracted / "campaign"
        campaign_path = campaign_root / "campaign.json"
        if _sha256_file(campaign_path) != manifest["campaignSha256"]:
            raise CampaignError("mutation campaign summary is not bound by the raw manifest")
        campaign = _strict_json(campaign_path)
        campaign_keys = {
            "schema",
            "type",
            "runToken",
            "scope",
            "fullGitSha",
            "checkoutStateDigest",
            "contractSha256",
            "startedAt",
            "endedAt",
            "attemptCount",
            "retryCount",
            "skipCount",
            "tools",
            "commands",
            "observations",
        }
        if (
            not isinstance(campaign, dict)
            or set(campaign) != campaign_keys
            or type(campaign.get("schema")) is not int
            or campaign["schema"] != 1
            or campaign.get("type") != "OwnerCriticalMutationCampaignV1"
            or campaign.get("runToken") != manifest["runToken"]
            or campaign.get("scope") != scope
            or campaign.get("fullGitSha") != manifest["fullGitSha"]
            or campaign.get("checkoutStateDigest") != manifest["checkoutStateDigest"]
            or campaign.get("contractSha256") != manifest["contractSha256"]
            or campaign.get("tools") != tools
            or type(campaign.get("attemptCount")) is not int
            or campaign["attemptCount"] != 1
            or type(campaign.get("retryCount")) is not int
            or campaign["retryCount"] != 0
            or type(campaign.get("skipCount")) is not int
            or campaign["skipCount"] != 0
        ):
            raise CampaignError("mutation campaign summary is malformed, retried, skipped, or substituted")
        started = _parse_utc(campaign.get("startedAt"), label="mutation campaign start")
        ended = _parse_utc(campaign.get("endedAt"), label="mutation campaign end")
        if ended < started:
            raise CampaignError("mutation campaign chronology runs backwards")
        _validate_command_records(campaign_root, campaign.get("commands"))
        observations = campaign.get("observations")
        if not isinstance(observations, dict) or set(observations) != set(scope):
            raise CampaignError("mutation campaign observations do not match its exact scope")
        recomputed: dict[str, Any] = {}
        if "frontend" in scope:
            raw = campaign_root / "frontend" / "raw"
            recomputed["frontend"] = _validate_frontend_raw(
                APP,
                contract,
                float(contract["thresholds"]["frontendReducerDomainPercent"]),
                report_path=raw / "stryker-report.json",
                event_directory=raw / "events",
                log_path=raw / "stryker.log",
            )
        if "backend" in scope:
            _sources, by_domain = _expanded_backend_sources(APP)
            recomputed["backend"] = _validate_backend_raw(
                inventory_path=campaign_root / "backend" / "inventory.json",
                native_inventory_path=campaign_root / "backend" / "raw" / "mutants.json",
                outcomes_path=campaign_root / "backend" / "raw" / "outcomes.json",
                app=APP,
                by_domain=by_domain,
                minimum=float(contract["thresholds"]["backendCriticalDomainPercent"]),
            )
        if _canonical_json(recomputed) != _canonical_json(observations):
            raise CampaignError("mutation campaign projection differs from independently replayed raw output")
    return {
        "fullGitSha": manifest["fullGitSha"],
        "scope": scope,
        "certificationEligible": manifest["certificationEligible"],
        "observations": recomputed,
        "manifestSha256": _sha256_file(manifest_path),
        "bundleSha256": _sha256_file(bundle_path),
    }


def _copy_frontend_raw(app: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=False)
    shutil.copy2(app / FRONTEND_REPORT, destination / "stryker-report.json")
    shutil.copytree(app / FRONTEND_EVENTS, destination / "events")
    shutil.copy2(app / FRONTEND_LOG, destination / "stryker.log")


def _clean_checkout() -> tuple[str, str]:
    full_sha = _run_text(["git", "rev-parse", "HEAD"], cwd=REPO)
    status = subprocess.run(
        ["git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cwd=REPO,
        check=True,
        shell=False,
        capture_output=True,
    ).stdout
    return full_sha, _sha256_bytes(status)


def _run_inventory_only(output: Path) -> None:
    contract = _load_contract()
    versions = _tool_versions(APP, contract)
    sources, by_domain = _expanded_backend_sources(APP)
    output.mkdir(parents=True, exist_ok=False)
    inventory = output / "backend-inventory.json"
    log = output / "backend-inventory.log"
    command = _backend_list_command(APP, sources)
    _run_split_logged(
        command,
        cwd=TAURI,
        stdout_path=inventory,
        logical_stdout_path="backend-inventory.json",
        stderr_path=log,
        logical_stderr_path="backend-inventory.log",
    )
    inventory_value = _strict_json(inventory)
    if not isinstance(inventory_value, list) or not inventory_value:
        raise CampaignError("cargo-mutants inventory discovered no critical mutants")
    _atomic_json(
        output / "diagnostic-summary.json",
        {
            "schema": 1,
            "type": "OwnerMutationInventoryDiagnosticV1",
            "certificationEligible": False,
            "reason": "inventory-only; no mutant outcomes were executed",
            "fullGitSha": _run_text(["git", "rev-parse", "HEAD"], cwd=REPO),
            "contractSha256": _sha256_file(CONTRACT_PATH),
            "tools": versions,
            "sources": sources,
            "domains": by_domain,
            "mutants": len(inventory_value),
            "artifacts": _artifact_inventory(output),
        },
    )


def _run_full(output: Path, *, frontend_only: bool, backend_only: bool) -> None:
    if frontend_only and backend_only:
        raise CampaignError("frontend-only and backend-only are mutually exclusive")
    full_sha, checkout_digest = _clean_checkout()
    dirty = _run_text(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=REPO)
    if dirty:
        raise CampaignError("certifying mutation campaign requires a completely clean checkout")
    contract = _load_contract()
    tools = _tool_versions(APP, contract)
    output.mkdir(parents=True, exist_ok=False)
    started = _utc_now()
    run_token = str(uuid.uuid4())
    scope = ["frontend"] if frontend_only else ["backend"] if backend_only else ["backend", "frontend"]
    temporary_parent = Path(tempfile.mkdtemp(prefix="cortex-owner-mutation-"))
    worktree = temporary_parent / "checkout"
    commands: list[dict[str, Any]] = []
    observations: dict[str, Any] = {}
    try:
        add = _run_logged(
            ["git", "worktree", "add", "--detach", str(worktree), full_sha],
            cwd=REPO,
            log_path=output / "setup" / "git-worktree.log",
            logical_log_path="setup/git-worktree.log",
        )
        commands.append(add)
        app = worktree / APP.relative_to(REPO)
        commands.append(
            _run_logged(
                [NPM, "ci", "--ignore-scripts"],
                cwd=app,
                log_path=output / "setup" / "npm-ci.log",
                logical_log_path="setup/npm-ci.log",
            )
        )
        _tool_versions(app, contract)
        if not backend_only:
            commands.append(
                _run_logged(
                    [NPM, "run", "test:mutation:frontend"],
                    cwd=app,
                    log_path=output / "frontend" / "run.log",
                    logical_log_path="frontend/run.log",
                    allowed_return_codes=tuple(contract["frontend"]["allowedExitCodes"]),
                )
            )
            frontend = _validate_frontend_raw(
                app,
                contract,
                float(contract["thresholds"]["frontendReducerDomainPercent"]),
            )
            _copy_frontend_raw(app, output / "frontend" / "raw")
            observations["frontend"] = frontend
        if not frontend_only:
            commands.append(
                _run_logged(
                    [NPM, "run", "build:vite"],
                    cwd=app,
                    log_path=output / "setup" / "frontend-build.log",
                    logical_log_path="setup/frontend-build.log",
                )
            )
            sources, by_domain = _expanded_backend_sources(app)
            backend_root = output / "backend"
            backend_root.mkdir(parents=True, exist_ok=False)
            inventory_path = backend_root / "inventory.json"
            inventory_log = backend_root / "inventory.log"
            inventory_command = _backend_list_command(app, sources)
            commands.append(
                _run_split_logged(
                    inventory_command,
                    cwd=app / "src-tauri",
                    stdout_path=inventory_path,
                    logical_stdout_path="backend/inventory.json",
                    stderr_path=inventory_log,
                    logical_stderr_path="backend/inventory.log",
                )
            )
            raw_output = backend_root / "raw"
            backend_command = _backend_run_command(app, sources, contract, raw_output)
            commands.append(
                _run_logged(
                    backend_command,
                    cwd=app / "src-tauri",
                    log_path=backend_root / "run.log",
                    logical_log_path="backend/run.log",
                    allowed_return_codes=tuple(contract["backend"]["allowedExitCodes"]),
                )
            )
            tracked_diff = subprocess.run(
                ["git", "diff", "--quiet", "--exit-code"],
                cwd=worktree,
                check=False,
                shell=False,
            ).returncode
            staged_diff = subprocess.run(
                ["git", "diff", "--cached", "--quiet", "--exit-code"],
                cwd=worktree,
                check=False,
                shell=False,
            ).returncode
            if tracked_diff != 0 or staged_diff != 0:
                raise CampaignError("cargo-mutants left the disposable source checkout mutated")
            observations["backend"] = _validate_backend_raw(
                inventory_path=inventory_path,
                native_inventory_path=raw_output / "mutants.json",
                outcomes_path=raw_output / "outcomes.json",
                app=app,
                by_domain=by_domain,
                minimum=float(contract["thresholds"]["backendCriticalDomainPercent"]),
            )
        ended = _utc_now()
        summary = {
            "schema": 1,
            "type": "OwnerCriticalMutationCampaignV1",
            "runToken": run_token,
            "scope": scope,
            "fullGitSha": full_sha,
            "checkoutStateDigest": checkout_digest,
            "contractSha256": _sha256_file(CONTRACT_PATH),
            "startedAt": started,
            "endedAt": ended,
            "attemptCount": 1,
            "retryCount": 0,
            "skipCount": 0,
            "tools": tools,
            "commands": commands,
            "observations": observations,
        }
        _atomic_json(output / "campaign.json", summary)
        _publish_raw_authority(
            output=output,
            app=app,
            full_sha=full_sha,
            checkout_digest=checkout_digest,
            run_token=run_token,
            scope=scope,
            tools=tools,
        )
        _atomic_json(output / "artifact-manifest.json", {"schema": 1, "artifacts": _artifact_inventory(output)})
    finally:
        if worktree.exists():
            subprocess.run(
                ["git", "worktree", "remove", "--force", str(worktree)],
                cwd=REPO,
                check=False,
                shell=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        resolved_parent = temporary_parent.resolve()
        temp_root = Path(tempfile.gettempdir()).resolve()
        if resolved_parent != temp_root and temp_root in resolved_parent.parents:
            shutil.rmtree(resolved_parent, ignore_errors=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--backend-inventory-only", action="store_true")
    parser.add_argument("--frontend-only", action="store_true")
    parser.add_argument("--backend-only", action="store_true")
    parser.add_argument("--replay", action="store_true")
    args = parser.parse_args(argv)
    try:
        output = args.output.resolve()
        if args.replay:
            if args.backend_inventory_only or args.frontend_only or args.backend_only:
                raise CampaignError("replay cannot be combined with a campaign selector")
            if not output.is_dir():
                raise CampaignError("replay output directory does not exist")
            result = _replay_raw_authority(output)
            print(json.dumps(result, ensure_ascii=False, sort_keys=True))
        elif output.exists():
            raise CampaignError("output must not already exist")
        elif args.backend_inventory_only:
            if args.frontend_only or args.backend_only:
                raise CampaignError("inventory-only cannot be combined with a full-campaign selector")
            _run_inventory_only(output)
        elif not args.replay:
            _run_full(output, frontend_only=args.frontend_only, backend_only=args.backend_only)
    except CampaignError as error:
        print(f"owner mutation campaign failed closed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
