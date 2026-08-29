#!/usr/bin/env python3
"""Fail-closed Rust coverage and production-module-size certification gates.

This module deliberately does not infer a PASS from a pretty ``cargo llvm-cov`` table.  The
coverage gate runs the exact all-target/all-feature branch-instrumented command, reads LLVM's JSON,
validates its shape, recomputes every percentage from integer counters, and refuses missing or
zero-denominator branch evidence.  A stable compiler/toolchain that cannot produce branch evidence
is therefore a certification failure, not an excuse to omit the metric.

The architecture gate counts physical production lines after removing only syntactically bounded
``#[cfg(test)]`` items.  Ordinary modules must remain below 2,000 production lines.  The sole escape
hatch is a path-specific, content-hash-bound ``immutable-history`` exception for a module at or above
the 2,500-line hard ceiling.  Wildcards, directory exceptions, stale hashes, and unused exceptions
are rejected.

Exit status 0 means the selected gate passed.  Any incomplete evidence or policy violation exits 1.
The JSON printed by this program is diagnostic evidence; the enclosing verifier remains responsible
for binding the invocation and artifact hash to an immutable release manifest.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Mapping, Sequence


APP_ROOT = Path(__file__).resolve().parents[1]
TAURI_ROOT = APP_ROOT / "src-tauri"
RUST_SOURCE_ROOT = TAURI_ROOT / "src"
DEFAULT_EXCEPTION_FILE = Path(__file__).with_name("rust_module_size_exceptions.json")
DEFAULT_COVERAGE_TOOLCHAIN_FILE = Path(__file__).with_name("rust_coverage_toolchain.json")

MIN_LINE_PERCENT = 85.0
MIN_REGION_PERCENT = 85.0  # LLVM regions are the available executable-statement proxy.
MIN_FUNCTION_PERCENT = 80.0
MIN_BRANCH_PERCENT = 80.0

CRITICAL_MIN_LINE_PERCENT = 95.0
CRITICAL_MIN_REGION_PERCENT = 95.0
CRITICAL_MIN_FUNCTION_PERCENT = 90.0
CRITICAL_MIN_BRANCH_PERCENT = 90.0

# These paths are the committed critical-domain boundary. A missing pattern is evidence failure,
# so moving critical code cannot silently remove it from the stricter coverage denominator.
CRITICAL_COVERAGE_DOMAINS: dict[str, tuple[str, ...]] = {
    "review": (
        "src-tauri/src/review_campaign.rs",
        "src-tauri/src/stores/review_write.rs",
        "src-tauri/src/commands/segments_read.rs",
        "src-tauri/src/commands/segments_write.rs",
    ),
    "payment": (
        "src-tauri/src/review_pool.rs",
        "src-tauri/src/review_pool_export.rs",
    ),
    "playback": (
        "src-tauri/src/media.rs",
        "src-tauri/src/technical_audio_probe.rs",
        "src-tauri/src/stores/review_write.rs",
    ),
    "restore": (
        "src-tauri/src/backup_service.rs",
        "src-tauri/src/recovery.rs",
        "src-tauri/src/snapshot.rs",
        "src-tauri/src/restore_service/*.rs",
    ),
    "ipc": (
        "src-tauri/src/ipc_contract.rs",
        "src-tauri/src/commands.rs",
        "src-tauri/src/commands/*.rs",
        "src-tauri/src/couch.rs",
        "src-tauri/src/http.rs",
    ),
}

# "below 2,000" is literal: 2,000 is already a violation.
NORMAL_MODULE_LIMIT = 2_000
HARD_MODULE_LIMIT = 2_500

REQUIRED_COVERAGE_FLAGS = (
    "llvm-cov",
    "--locked",
    "--all-targets",
    "--all-features",
    "--branch",
    "--json",
)


class GateError(RuntimeError):
    """Evidence is malformed, incomplete, or cannot be obtained."""


@dataclass(frozen=True)
class CoverageToolchainContract:
    toolchain: str
    host: str
    rustc_release: str
    rustc_commit_hash: str
    rustc_commit_date: str
    llvm_version: str
    cargo_release: str
    cargo_commit_hash: str
    cargo_commit_date: str
    cargo_llvm_cov_version: str
    contract_sha256: str


@dataclass(frozen=True)
class CoverageToolchainIdentity:
    contract_sha256: str
    toolchain: str
    host: str
    rustc_version: str
    rustc_commit_hash: str
    rustc_commit_date: str
    llvm_version: str
    cargo_version: str
    cargo_commit_hash: str
    cargo_commit_date: str
    cargo_llvm_cov_version: str


@dataclass(frozen=True)
class CoverageExecution:
    verdict: "CoverageVerdict"
    toolchain: CoverageToolchainIdentity


@dataclass(frozen=True)
class CoverageMetric:
    count: int
    covered: int
    percent: float
    required_percent: float

    @property
    def passed(self) -> bool:
        return self.percent + 1e-12 >= self.required_percent


@dataclass(frozen=True)
class CoverageVerdict:
    passed: bool
    artifact_sha256: str
    metrics: dict[str, CoverageMetric]
    critical_domains: dict[str, "CriticalCoverageDomain"]
    failures: tuple[str, ...]


@dataclass(frozen=True)
class CriticalCoverageDomain:
    patterns: tuple[str, ...]
    matched_files: tuple[str, ...]
    metrics: dict[str, CoverageMetric]
    passed: bool


@dataclass(frozen=True)
class ModuleMeasurement:
    path: str
    physical_lines: int
    test_only_lines: int
    production_lines: int
    sha256: str
    exception: str | None


@dataclass(frozen=True)
class ArchitectureVerdict:
    passed: bool
    scanned_modules: int
    measurements: tuple[ModuleMeasurement, ...]
    failures: tuple[str, ...]


@dataclass(frozen=True)
class ModuleException:
    path: str
    kind: str
    sha256: str
    max_production_lines: int
    reason: str
    basis: str


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _exact_string(value: Any, field: str, pattern: str) -> str:
    if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
        raise GateError(f"coverage toolchain field {field!r} is malformed")
    return value


def load_coverage_toolchain_contract(
    path: Path = DEFAULT_COVERAGE_TOOLCHAIN_FILE,
) -> CoverageToolchainContract:
    """Load the exact nightly authority without changing the production Rust toolchain.

    The repository-root ``rust-toolchain.toml`` remains the production compiler contract.  This
    separate, hash-bound document exists only because LLVM branch instrumentation requires nightly.
    A rolling ``nightly`` selector would make otherwise identical proof runs non-reproducible.
    """

    try:
        payload = path.read_bytes()
        document = json.loads(payload.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GateError(f"coverage toolchain contract cannot be read as UTF-8 JSON: {error}") from error
    if not isinstance(document, Mapping) or set(document) != {
        "schema",
        "toolchain",
        "host",
        "rustc",
        "cargo",
        "cargoLlvmCovVersion",
    }:
        raise GateError("coverage toolchain contract has missing or unknown fields")
    if document["schema"] != 1 or isinstance(document["schema"], bool):
        raise GateError("coverage toolchain contract schema must be integer 1")
    rustc = document["rustc"]
    cargo = document["cargo"]
    if not isinstance(rustc, Mapping) or set(rustc) != {
        "release",
        "commitHash",
        "commitDate",
        "llvmVersion",
    }:
        raise GateError("coverage toolchain rustc identity has missing or unknown fields")
    if not isinstance(cargo, Mapping) or set(cargo) != {"release", "commitHash", "commitDate"}:
        raise GateError("coverage toolchain cargo identity has missing or unknown fields")
    return CoverageToolchainContract(
        toolchain=_exact_string(document["toolchain"], "toolchain", r"nightly-\d{4}-\d{2}-\d{2}"),
        host=_exact_string(document["host"], "host", r"[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?"),
        rustc_release=_exact_string(rustc["release"], "rustc.release", r"\d+\.\d+\.\d+-nightly"),
        rustc_commit_hash=_exact_string(rustc["commitHash"], "rustc.commitHash", r"[0-9a-f]{40}"),
        rustc_commit_date=_exact_string(rustc["commitDate"], "rustc.commitDate", r"\d{4}-\d{2}-\d{2}"),
        llvm_version=_exact_string(rustc["llvmVersion"], "rustc.llvmVersion", r"\d+\.\d+\.\d+"),
        cargo_release=_exact_string(cargo["release"], "cargo.release", r"\d+\.\d+\.\d+-nightly"),
        cargo_commit_hash=_exact_string(cargo["commitHash"], "cargo.commitHash", r"[0-9a-f]{40}"),
        cargo_commit_date=_exact_string(cargo["commitDate"], "cargo.commitDate", r"\d{4}-\d{2}-\d{2}"),
        cargo_llvm_cov_version=_exact_string(
            document["cargoLlvmCovVersion"], "cargoLlvmCovVersion", r"\d+\.\d+\.\d+"
        ),
        contract_sha256=_sha256_bytes(payload),
    )


def _probe(command: Sequence[str], environment: Mapping[str, str], label: str) -> str:
    try:
        completed = subprocess.run(
            list(command),
            cwd=TAURI_ROOT,
            env=dict(environment),
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
            shell=False,
        )
    except FileNotFoundError as error:
        raise GateError(f"coverage toolchain probe is unavailable: {label}") from error
    except (OSError, subprocess.SubprocessError) as error:
        raise GateError(f"coverage toolchain probe failed for {label}: {error}") from error
    output = ((completed.stdout or "") + "\n" + (completed.stderr or "")).strip()
    if completed.returncode != 0 or not output or len(output) > 4096:
        raise GateError(f"coverage toolchain identity is unprovable for {label}")
    return output


def _version_fields(output: str, label: str) -> tuple[str, dict[str, str]]:
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if not lines:
        raise GateError(f"coverage toolchain {label} version is empty")
    fields: dict[str, str] = {}
    for line in lines[1:]:
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        key = key.strip()
        if key in fields:
            raise GateError(f"coverage toolchain {label} repeats identity field {key!r}")
        fields[key] = value.strip()
    return lines[0], fields


def expected_coverage_toolchain_identity(
    path: Path = DEFAULT_COVERAGE_TOOLCHAIN_FILE,
) -> CoverageToolchainIdentity:
    contract = load_coverage_toolchain_contract(path)
    return CoverageToolchainIdentity(
        contract_sha256=contract.contract_sha256,
        toolchain=contract.toolchain,
        host=contract.host,
        rustc_version=(
            f"rustc {contract.rustc_release} "
            f"({contract.rustc_commit_hash[:9]} {contract.rustc_commit_date})"
        ),
        rustc_commit_hash=contract.rustc_commit_hash,
        rustc_commit_date=contract.rustc_commit_date,
        llvm_version=contract.llvm_version,
        cargo_version=(
            f"cargo {contract.cargo_release} "
            f"({contract.cargo_commit_hash[:9]} {contract.cargo_commit_date})"
        ),
        cargo_commit_hash=contract.cargo_commit_hash,
        cargo_commit_date=contract.cargo_commit_date,
        cargo_llvm_cov_version=contract.cargo_llvm_cov_version,
    )


def verify_coverage_toolchain(
    *,
    contract_path: Path = DEFAULT_COVERAGE_TOOLCHAIN_FILE,
    environment: Mapping[str, str] | None = None,
) -> CoverageToolchainIdentity:
    """Prove that the installed measurement compiler exactly matches the committed authority."""

    contract = load_coverage_toolchain_contract(contract_path)
    expected_identity = expected_coverage_toolchain_identity(contract_path)
    probe_environment = dict(os.environ if environment is None else environment)
    selector = f"+{contract.toolchain}"
    rustc_output = _probe(["rustc", selector, "-Vv"], probe_environment, "rustc")
    cargo_output = _probe(["cargo", selector, "--version", "--verbose"], probe_environment, "cargo")
    llvm_cov_output = _probe(
        ["cargo", selector, "llvm-cov", "--version"], probe_environment, "cargo-llvm-cov"
    )
    rustc_version, rustc_fields = _version_fields(rustc_output, "rustc")
    cargo_version, cargo_fields = _version_fields(cargo_output, "cargo")
    mismatches = {
        "rustc.version": (rustc_version, expected_identity.rustc_version),
        "rustc.commit-hash": (rustc_fields.get("commit-hash"), contract.rustc_commit_hash),
        "rustc.commit-date": (rustc_fields.get("commit-date"), contract.rustc_commit_date),
        "rustc.host": (rustc_fields.get("host"), contract.host),
        "rustc.release": (rustc_fields.get("release"), contract.rustc_release),
        "rustc.LLVM version": (rustc_fields.get("LLVM version"), contract.llvm_version),
        "cargo.version": (cargo_version, expected_identity.cargo_version),
        "cargo.commit-hash": (cargo_fields.get("commit-hash"), contract.cargo_commit_hash),
        "cargo.commit-date": (cargo_fields.get("commit-date"), contract.cargo_commit_date),
        "cargo.host": (cargo_fields.get("host"), contract.host),
        "cargo.release": (cargo_fields.get("release"), contract.cargo_release),
        "cargo-llvm-cov.version": (
            llvm_cov_output,
            f"cargo-llvm-cov {contract.cargo_llvm_cov_version}",
        ),
    }
    wrong = [f"{name}: observed={observed!r}, expected={expected!r}" for name, (observed, expected) in mismatches.items() if observed != expected]
    if wrong:
        raise GateError("coverage toolchain does not match its exact contract: " + "; ".join(wrong))
    return expected_identity


def _strict_int(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise GateError(f"coverage field {field!r} must be an integer")
    return value


def _strict_number(value: Any, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise GateError(f"coverage field {field!r} must be numeric")
    result = float(value)
    if not math.isfinite(result):
        raise GateError(f"coverage field {field!r} must be finite")
    return result


def _metric(
    totals: Mapping[str, Any],
    name: str,
    required_percent: float,
) -> CoverageMetric:
    raw = totals.get(name)
    if not isinstance(raw, Mapping):
        raise GateError(f"LLVM coverage totals are missing the {name!r} metric")
    count = _strict_int(raw.get("count"), f"{name}.count")
    covered = _strict_int(raw.get("covered"), f"{name}.covered")
    if count <= 0:
        raise GateError(f"LLVM {name} evidence has a zero denominator; this metric is unproven")
    if covered < 0 or covered > count:
        raise GateError(f"LLVM {name} counters are impossible: covered={covered}, count={count}")

    recomputed = covered * 100.0 / count
    # LLVM currently emits `percent`, but the integer counters are the authority.  If a producer
    # includes a percentage, ensure it is not lying before ignoring it in favour of recomputation.
    if "percent" in raw:
        claimed = _strict_number(raw["percent"], f"{name}.percent")
        if abs(claimed - recomputed) > 0.02:
            raise GateError(
                f"LLVM {name} percent disagrees with its counters: claimed={claimed:.6f}, "
                f"recomputed={recomputed:.6f}"
            )
    return CoverageMetric(count=count, covered=covered, percent=recomputed, required_percent=required_percent)


def _coverage_source_path(filename: Any) -> str | None:
    """Return a stable repository-relative Rust source path from LLVM's host path."""

    if not isinstance(filename, str) or not filename or "\x00" in filename:
        raise GateError("LLVM coverage file has a malformed filename")
    normalized = filename.replace("\\", "/")
    folded = normalized.casefold()
    marker = "/src-tauri/src/"
    marker_index = folded.rfind(marker)
    if marker_index >= 0:
        relative = "src-tauri/src/" + normalized[marker_index + len(marker) :]
    elif folded.startswith("src-tauri/src/"):
        relative = normalized
    else:
        return None
    while "//" in relative:
        relative = relative.replace("//", "/")
    parts = PurePosixPath(relative).parts
    if not parts or any(part in {"", ".", ".."} for part in parts):
        raise GateError(f"LLVM coverage file path is not canonical: {filename!r}")
    return PurePosixPath(*parts).as_posix().casefold()


def _metric_counters(raw_totals: Mapping[str, Any], name: str, label: str) -> tuple[int, int]:
    raw = raw_totals.get(name)
    if not isinstance(raw, Mapping):
        raise GateError(f"LLVM coverage summary for {label} is missing the {name!r} metric")
    count = _strict_int(raw.get("count"), f"{label}.{name}.count")
    covered = _strict_int(raw.get("covered"), f"{label}.{name}.covered")
    if count < 0 or covered < 0 or covered > count:
        raise GateError(
            f"LLVM {name} counters are impossible for {label}: covered={covered}, count={count}"
        )
    recomputed = covered * 100.0 / count if count else 0.0
    if "percent" in raw:
        claimed = _strict_number(raw["percent"], f"{label}.{name}.percent")
        if abs(claimed - recomputed) > 0.02:
            raise GateError(
                f"LLVM {name} percent disagrees with its counters for {label}: "
                f"claimed={claimed:.6f}, recomputed={recomputed:.6f}"
            )
    return count, covered


def _critical_coverage_domains(data_set: Mapping[str, Any]) -> tuple[
    dict[str, CriticalCoverageDomain], tuple[str, ...]
]:
    raw_files = data_set.get("files")
    if not isinstance(raw_files, list) or not raw_files:
        raise GateError("LLVM coverage artifact has no per-file evidence for critical domains")

    summaries: dict[str, Mapping[str, Any]] = {}
    for index, raw_file in enumerate(raw_files):
        if not isinstance(raw_file, Mapping):
            raise GateError(f"LLVM coverage file entry {index} is malformed")
        relative = _coverage_source_path(raw_file.get("filename"))
        if relative is None:
            continue
        if relative in summaries:
            raise GateError(f"LLVM coverage artifact repeats critical-source identity {relative}")
        summary = raw_file.get("summary")
        if not isinstance(summary, Mapping):
            raise GateError(f"LLVM coverage file {relative} has no summary")
        summaries[relative] = summary

    domains: dict[str, CriticalCoverageDomain] = {}
    failures: list[str] = []
    thresholds = {
        "lines": CRITICAL_MIN_LINE_PERCENT,
        "regions": CRITICAL_MIN_REGION_PERCENT,
        "functions": CRITICAL_MIN_FUNCTION_PERCENT,
        "branches": CRITICAL_MIN_BRANCH_PERCENT,
    }
    for domain, patterns in CRITICAL_COVERAGE_DOMAINS.items():
        matched: set[str] = set()
        missing_patterns: list[str] = []
        for pattern in patterns:
            pattern_matches = {
                path for path in summaries if fnmatch.fnmatchcase(path, pattern.casefold())
            }
            if not pattern_matches:
                missing_patterns.append(pattern)
            matched.update(pattern_matches)
        if missing_patterns:
            failures.extend(
                f"critical {domain} coverage pattern has no LLVM file evidence: {pattern}"
                for pattern in missing_patterns
            )

        metrics: dict[str, CoverageMetric] = {}
        for name, required_percent in thresholds.items():
            count = 0
            covered = 0
            for path in sorted(matched):
                file_count, file_covered = _metric_counters(summaries[path], name, path)
                count += file_count
                covered += file_covered
            if count <= 0:
                failures.append(
                    f"critical {domain} {name} evidence has a zero denominator; this metric is unproven"
                )
                metric = CoverageMetric(
                    count=count,
                    covered=covered,
                    percent=0.0,
                    required_percent=required_percent,
                )
            else:
                metric = CoverageMetric(
                    count=count,
                    covered=covered,
                    percent=covered * 100.0 / count,
                    required_percent=required_percent,
                )
                if not metric.passed:
                    failures.append(
                        f"critical {domain} {name} coverage {metric.percent:.2f}% is below "
                        f"{required_percent:.2f}%"
                    )
            metrics[name] = metric
        domains[domain] = CriticalCoverageDomain(
            patterns=patterns,
            matched_files=tuple(sorted(matched)),
            metrics=metrics,
            passed=(
                not missing_patterns
                and all(metric.count > 0 and metric.passed for metric in metrics.values())
            ),
        )
    return domains, tuple(failures)


def parse_llvm_coverage(payload: bytes) -> CoverageVerdict:
    """Validate one LLVM JSON export and evaluate the locked product thresholds."""

    artifact_sha256 = _sha256_bytes(payload)
    try:
        document = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GateError(f"coverage artifact is not canonical UTF-8 JSON: {error}") from error
    if not isinstance(document, Mapping):
        raise GateError("coverage artifact root must be an object")
    if document.get("type") != "llvm.coverage.json.export":
        raise GateError("coverage artifact is not an LLVM coverage JSON export")
    data = document.get("data")
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], Mapping):
        raise GateError("coverage artifact must contain exactly one unambiguous LLVM data set")
    totals = data[0].get("totals")
    if not isinstance(totals, Mapping):
        raise GateError("coverage artifact has no aggregate totals")

    metrics = {
        "lines": _metric(totals, "lines", MIN_LINE_PERCENT),
        "regions": _metric(totals, "regions", MIN_REGION_PERCENT),
        "functions": _metric(totals, "functions", MIN_FUNCTION_PERCENT),
        "branches": _metric(totals, "branches", MIN_BRANCH_PERCENT),
    }
    failures = tuple(
        f"{name} coverage {metric.percent:.2f}% is below {metric.required_percent:.2f}%"
        for name, metric in metrics.items()
        if not metric.passed
    )
    critical_domains, critical_failures = _critical_coverage_domains(data[0])
    failures += critical_failures
    return CoverageVerdict(
        passed=not failures,
        artifact_sha256=artifact_sha256,
        metrics=metrics,
        critical_domains=critical_domains,
        failures=failures,
    )


def validate_coverage_file(path: Path) -> CoverageVerdict:
    try:
        payload = path.read_bytes()
    except OSError as error:
        raise GateError(f"coverage artifact cannot be read from {path}: {error}") from error
    if not payload:
        raise GateError(f"coverage artifact is empty: {path}")
    return parse_llvm_coverage(payload)


def _coverage_command(
    output_path: Path,
    contract: CoverageToolchainContract | None = None,
) -> list[str]:
    # Keep this as an argument array.  Shell interpretation would make the evidence command
    # platform-dependent and permit metacharacters in a path to change what was measured.
    authority = contract or load_coverage_toolchain_contract()
    return [
        "cargo",
        f"+{authority.toolchain}",
        *REQUIRED_COVERAGE_FLAGS,
        "--output-path",
        str(output_path),
    ]


def run_coverage(*, output_path: Path, timeout_seconds: int) -> CoverageExecution:
    """Run the exact certification measurement, then validate the artifact it wrote."""

    if timeout_seconds <= 0:
        raise GateError("coverage timeout must be a positive number of seconds")
    output_path = output_path.resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    # A stale JSON file must never survive a crashed/unsupported fresh measurement and get parsed.
    try:
        output_path.unlink(missing_ok=True)
    except OSError as error:
        raise GateError(f"stale coverage artifact cannot be removed: {error}") from error

    environment = os.environ.copy()
    environment["CARGO_INCREMENTAL"] = "0"
    environment["CARGO_NET_OFFLINE"] = "true"
    environment["CARGO_TERM_COLOR"] = "never"
    identity = verify_coverage_toolchain(environment=environment)
    contract = load_coverage_toolchain_contract()
    command = _coverage_command(output_path, contract)
    try:
        completed = subprocess.run(
            command,
            cwd=TAURI_ROOT,
            env=environment,
            check=False,
            timeout=timeout_seconds,
        )
    except FileNotFoundError as error:
        raise GateError("cargo/cargo-llvm-cov is unavailable; Rust coverage is unproven") from error
    except subprocess.TimeoutExpired as error:
        raise GateError(f"Rust coverage exceeded its explicit {timeout_seconds}s timeout") from error
    if completed.returncode != 0:
        raise GateError(
            "branch-instrumented cargo llvm-cov failed; certification cannot fall back to a "
            f"branch-free report (exit {completed.returncode})"
        )
    if not output_path.is_file():
        raise GateError("cargo llvm-cov exited successfully without writing its required JSON artifact")
    return CoverageExecution(verdict=validate_coverage_file(output_path), toolchain=identity)


def _mask_rust_non_code(source: str) -> str:
    """Mask comments and literals while preserving offsets, newlines, and Rust punctuation."""

    chars = list(source)

    def blank(start: int, end: int) -> None:
        for index in range(start, end):
            if chars[index] not in "\r\n":
                chars[index] = " "

    length = len(source)
    index = 0
    while index < length:
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            if end < 0:
                end = length
            blank(index, end)
            index = end
            continue
        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < length and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            if depth:
                raise GateError("Rust source contains an unterminated block comment")
            blank(index, end)
            index = end
            continue

        raw_prefix_length = 0
        if source[index] == "r":
            raw_prefix_length = 1
        elif source.startswith("br", index):
            raw_prefix_length = 2
        hash_end = index + raw_prefix_length
        while raw_prefix_length and hash_end < length and source[hash_end] == "#" and hash_end - index <= 255:
            hash_end += 1
        if raw_prefix_length and hash_end < length and source[hash_end] == '"':
            hashes = source[index + raw_prefix_length : hash_end]
            terminator = '"' + hashes
            content_start = hash_end + 1
            close = source.find(terminator, content_start)
            if close < 0:
                raise GateError("Rust source contains an unterminated raw string")
            end = close + len(terminator)
            blank(index, end)
            index = end
            continue

        quote_index = index + 1 if source.startswith('b"', index) else index
        if quote_index < length and source[quote_index] == '"':
            end = quote_index + 1
            escaped = False
            while end < length:
                char = source[end]
                if char == '"' and not escaped:
                    end += 1
                    break
                if char == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
                end += 1
            else:
                raise GateError("Rust source contains an unterminated string literal")
            blank(index, end)
            index = end
            continue

        # A lifetime (`'a`, `'static`) has no closing quote.  Mask only a same-line, no-whitespace
        # sequence that actually closes, which covers ordinary and escaped Rust char literals.
        if source[index] == "'":
            end = index + 1
            escaped = False
            closed = False
            while end < length and source[end] not in "\r\n\t ":
                char = source[end]
                if char == "'" and not escaped:
                    end += 1
                    closed = True
                    break
                if char == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
                end += 1
            if closed:
                blank(index, end)
                index = end
                continue
        index += 1
    return "".join(chars)


_CFG_TEST = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def _matching_delimiter(masked: str, start: int, opening: str, closing: str) -> int:
    depth = 0
    for index in range(start, len(masked)):
        char = masked[index]
        if char == opening:
            depth += 1
        elif char == closing:
            depth -= 1
            if depth == 0:
                return index + 1
    raise GateError(f"Rust cfg(test) item has an unmatched {opening!r} delimiter")


def _cfg_test_item_end(masked: str, attribute_end: int) -> int:
    cursor = attribute_end
    length = len(masked)
    while True:
        while cursor < length and masked[cursor].isspace():
            cursor += 1
        if not masked.startswith("#[", cursor):
            break
        close = _matching_delimiter(masked, cursor + 1, "[", "]")
        cursor = close

    parens = 0
    brackets = 0
    while cursor < length:
        char = masked[cursor]
        if char == "(":
            parens += 1
        elif char == ")":
            parens -= 1
        elif char == "[":
            brackets += 1
        elif char == "]":
            brackets -= 1
        elif parens == 0 and brackets == 0 and char == ";":
            return cursor + 1
        elif parens == 0 and brackets == 0 and char == "{":
            end = _matching_delimiter(masked, cursor, "{", "}")
            while end < length and masked[end].isspace() and masked[end] not in "\r\n":
                end += 1
            if end < length and masked[end] == ";":
                end += 1
            return end
        cursor += 1
    raise GateError("Rust cfg(test) attribute is not followed by a bounded item")


def cfg_test_item_spans(source: str) -> tuple[tuple[int, int], ...]:
    """Offset spans occupied by syntactically bounded ``#[cfg(test)]`` items."""

    masked = _mask_rust_non_code(source)
    spans: list[tuple[int, int]] = []
    consumed_until = -1
    for match in _CFG_TEST.finditer(masked):
        if match.start() < consumed_until:
            continue
        end = _cfg_test_item_end(masked, match.end())
        spans.append((match.start(), end))
        consumed_until = end
    return tuple(spans)


def cfg_test_line_numbers(source: str) -> set[int]:
    """One-based physical lines occupied by syntactically bounded cfg(test) items."""

    excluded: set[int] = set()
    for start, end in cfg_test_item_spans(source):
        first = source.count("\n", 0, start) + 1
        last = source.count("\n", 0, max(start, end - 1)) + 1
        excluded.update(range(first, last + 1))
    return excluded


def production_line_count(source: str) -> tuple[int, int, int]:
    # A trailing newline terminates the last physical line; it does not create a phantom empty line.
    physical = source.count("\n") + (1 if source and not source.endswith("\n") else 0)
    test_lines = len(cfg_test_line_numbers(source))
    if test_lines > physical:
        raise GateError("internal cfg(test) line accounting exceeded the physical file size")
    return physical, test_lines, physical - test_lines


def _safe_relative_path(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{field} must be a non-empty relative path")
    if "\\" in value or any(token in value for token in ("*", "?", "[", "]")):
        raise GateError(f"{field} must be one exact POSIX path without wildcards")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise GateError(f"{field} escapes or ambiguously names the app root")
    return path.as_posix()


def load_module_exceptions(path: Path) -> dict[str, ModuleException]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GateError(f"module exception registry cannot be read as UTF-8 JSON: {error}") from error
    if not isinstance(document, Mapping) or set(document) != {"schema", "exceptions"}:
        raise GateError("module exception registry must contain exactly schema and exceptions")
    if document["schema"] != 1 or isinstance(document["schema"], bool):
        raise GateError("module exception registry schema must be integer 1")
    rows = document["exceptions"]
    if not isinstance(rows, list):
        raise GateError("module exception registry exceptions must be a list")

    expected_keys = {"path", "kind", "sha256", "max_production_lines", "reason", "basis"}
    result: dict[str, ModuleException] = {}
    for index, row in enumerate(rows):
        if not isinstance(row, Mapping) or set(row) != expected_keys:
            raise GateError(f"module exception {index} has missing or unknown fields")
        exact_path = _safe_relative_path(row["path"], f"exceptions[{index}].path")
        if exact_path in result:
            raise GateError(f"duplicate module exception for {exact_path}")
        if row["kind"] != "immutable-history":
            raise GateError(f"module exception {exact_path} has unsupported kind {row['kind']!r}")
        digest = row["sha256"]
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise GateError(f"module exception {exact_path} needs a lowercase SHA-256")
        maximum = row["max_production_lines"]
        if isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < HARD_MODULE_LIMIT:
            raise GateError(f"module exception {exact_path} max must be at least the hard ceiling")
        reason = row["reason"]
        basis = row["basis"]
        if not isinstance(reason, str) or len(reason.strip()) < 40:
            raise GateError(f"module exception {exact_path} needs a substantive reason")
        if not isinstance(basis, str) or len(basis.strip()) < 20:
            raise GateError(f"module exception {exact_path} needs a reviewable policy basis")
        result[exact_path] = ModuleException(exact_path, row["kind"], digest, maximum, reason, basis)
    return result


def _declared_test_only_modules(source_root: Path) -> set[Path]:
    """Resolve only external modules whose declaration itself is cfg(test)-bounded.

    A filename such as ``important_tests.rs`` is not evidence that Rust excludes it from the binary.
    Requiring the real module declaration prevents a suffix/directory naming trick from hiding shipped
    production code from the size gate.
    """

    excluded: set[Path] = set()
    for owner in sorted(source_root.rglob("*.rs")):
        try:
            source = owner.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise GateError(f"Rust module cannot be read as UTF-8: {owner}: {error}") from error
        for start, end in cfg_test_item_spans(source):
            item = source[start:end]
            module = re.search(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;", item)
            if module is None:
                continue
            path_attribute = re.search(r"#\s*\[\s*path\s*=\s*\"([^\"]+)\"\s*\]", item)
            if path_attribute:
                candidate = owner.parent / path_attribute.group(1)
                candidates = (candidate,)
            else:
                name = module.group(1)
                candidates = (owner.parent / f"{name}.rs", owner.parent / name / "mod.rs")
            for candidate in candidates:
                resolved = candidate.resolve()
                try:
                    resolved.relative_to(source_root.resolve())
                except ValueError as error:
                    raise GateError(f"cfg(test) module path escapes the Rust source root: {candidate}") from error
                if resolved.is_file():
                    excluded.add(resolved)
    return excluded


def _production_rust_files(source_root: Path) -> Iterable[Path]:
    test_only_modules = _declared_test_only_modules(source_root)
    for path in sorted(source_root.rglob("*.rs")):
        # Integration tests live outside src. External unit-test modules are excluded only when a
        # real, syntactically bounded cfg(test) declaration points at that exact resolved file.
        if path.resolve() in test_only_modules:
            continue
        yield path


def evaluate_architecture(
    *,
    app_root: Path = APP_ROOT,
    exception_file: Path = DEFAULT_EXCEPTION_FILE,
) -> ArchitectureVerdict:
    source_root = app_root / "src-tauri" / "src"
    if not source_root.is_dir():
        raise GateError(f"Rust source root does not exist: {source_root}")
    exceptions = load_module_exceptions(exception_file)
    used_exceptions: set[str] = set()
    measurements: list[ModuleMeasurement] = []
    failures: list[str] = []

    for path in _production_rust_files(source_root):
        try:
            payload = path.read_bytes()
            source = payload.decode("utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise GateError(f"Rust module cannot be read as UTF-8: {path}: {error}") from error
        relative = path.relative_to(app_root).as_posix()
        physical, test_only, production = production_line_count(source)
        digest = _sha256_bytes(payload)
        exception = exceptions.get(relative)
        exception_state: str | None = None

        if exception is not None:
            used_exceptions.add(relative)
            if production < HARD_MODULE_LIMIT:
                failures.append(
                    f"{relative}: immutable-history exception is not allowed below the "
                    f"{HARD_MODULE_LIMIT}-line hard ceiling ({production} production lines)"
                )
                exception_state = "ineligible"
            elif digest != exception.sha256:
                failures.append(
                    f"{relative}: immutable-history exception hash is stale "
                    f"(expected {exception.sha256}, observed {digest})"
                )
                exception_state = "hash-mismatch"
            elif production > exception.max_production_lines:
                failures.append(
                    f"{relative}: immutable history grew to {production} lines beyond its reviewed "
                    f"maximum {exception.max_production_lines}"
                )
                exception_state = "maximum-exceeded"
            else:
                exception_state = "immutable-history"

        if production >= NORMAL_MODULE_LIMIT and exception_state != "immutable-history":
            severity = "hard-ceiling" if production >= HARD_MODULE_LIMIT else "normal-ceiling"
            failures.append(
                f"{relative}: {production} production lines violates the below-{NORMAL_MODULE_LIMIT} "
                f"module policy ({severity})"
            )
        measurements.append(
            ModuleMeasurement(
                path=relative,
                physical_lines=physical,
                test_only_lines=test_only,
                production_lines=production,
                sha256=digest,
                exception=exception_state,
            )
        )

    unused = sorted(set(exceptions) - used_exceptions)
    failures.extend(f"unused module exception does not name a scanned Rust module: {path}" for path in unused)
    measurements.sort(key=lambda row: (-row.production_lines, row.path))
    return ArchitectureVerdict(
        passed=not failures,
        scanned_modules=len(measurements),
        measurements=tuple(measurements),
        failures=tuple(failures),
    )


def _coverage_json(verdict: CoverageVerdict) -> dict[str, Any]:
    return {
        "schema": 1,
        "gate": "rust-coverage",
        "passed": verdict.passed,
        "artifactSha256": verdict.artifact_sha256,
        "metrics": {name: asdict(metric) for name, metric in verdict.metrics.items()},
        "criticalDomains": {
            name: {
                "patterns": list(domain.patterns),
                "matchedFiles": list(domain.matched_files),
                "metrics": {
                    metric_name: asdict(metric)
                    for metric_name, metric in domain.metrics.items()
                },
                "passed": domain.passed,
            }
            for name, domain in verdict.critical_domains.items()
        },
        "failures": list(verdict.failures),
    }


def _architecture_json(verdict: ArchitectureVerdict) -> dict[str, Any]:
    return {
        "schema": 1,
        "gate": "rust-module-size",
        "passed": verdict.passed,
        "normalModuleLimitExclusive": NORMAL_MODULE_LIMIT,
        "hardModuleLimit": HARD_MODULE_LIMIT,
        "scannedModules": verdict.scanned_modules,
        "measurements": [asdict(row) for row in verdict.measurements],
        "failures": list(verdict.failures),
    }


def _emit(document: Mapping[str, Any]) -> None:
    print(json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    coverage = commands.add_parser("coverage", help="run and validate exact branch-instrumented Rust coverage")
    coverage.add_argument(
        "--output",
        type=Path,
        default=TAURI_ROOT / "target" / "cortex-rust-coverage.json",
        help="durable LLVM JSON artifact path",
    )
    coverage.add_argument("--timeout-seconds", type=int, default=7_200)

    validate = commands.add_parser("coverage-json", help="diagnostically validate an existing LLVM JSON artifact")
    validate.add_argument("artifact", type=Path)

    architecture = commands.add_parser("architecture", help="measure production Rust module sizes")
    architecture.add_argument("--app-root", type=Path, default=APP_ROOT)
    architecture.add_argument("--exceptions", type=Path, default=DEFAULT_EXCEPTION_FILE)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "coverage":
            execution = run_coverage(output_path=args.output, timeout_seconds=args.timeout_seconds)
            document = _coverage_json(execution.verdict)
            document["toolchain"] = asdict(execution.toolchain)
            _emit(document)
            return 0 if execution.verdict.passed else 1
        if args.command == "coverage-json":
            verdict = validate_coverage_file(args.artifact)
            document = _coverage_json(verdict)
            document["diagnosticOnly"] = True
            document["certifyingInvocationProven"] = False
            document["toolchain"] = None
            _emit(document)
            return 0 if verdict.passed else 1
        if args.command == "architecture":
            verdict = evaluate_architecture(app_root=args.app_root.resolve(), exception_file=args.exceptions.resolve())
            _emit(_architecture_json(verdict))
            return 0 if verdict.passed else 1
        raise GateError(f"unsupported gate command: {args.command}")
    except GateError as error:
        _emit({"schema": 1, "gate": args.command, "passed": False, "evidenceError": str(error)})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
