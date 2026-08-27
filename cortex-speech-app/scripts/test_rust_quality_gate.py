#!/usr/bin/env python3
"""Adversarial regressions for the fail-closed Rust quality gates."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import rust_quality_gate as gate


def llvm_payload(
    *,
    lines: tuple[int, int] = (100, 85),
    regions: tuple[int, int] = (100, 85),
    functions: tuple[int, int] = (100, 80),
    branches: tuple[int, int] | None = (100, 80),
) -> bytes:
    totals: dict[str, object] = {}
    for name, values in (
        ("lines", lines),
        ("regions", regions),
        ("functions", functions),
        ("branches", branches),
    ):
        if values is None:
            continue
        count, covered = values
        totals[name] = {
            "count": count,
            "covered": covered,
            "percent": covered * 100.0 / count if count else 0,
        }
    critical_files: list[dict[str, object]] = []
    representative_paths = {
        pattern.replace("*.rs", "fixture.rs")
        for patterns in gate.CRITICAL_COVERAGE_DOMAINS.values()
        for pattern in patterns
    }
    for path in sorted(representative_paths):
        summary: dict[str, object] = {}
        for name, values in (
            ("lines", lines),
            ("regions", regions),
            ("functions", functions),
            ("branches", branches),
        ):
            if values is None:
                continue
            count, _ = values
            # Critical domains intentionally have stricter thresholds than the global fixture.
            required = 95 if name in {"lines", "regions", "functions"} else 90
            covered = min(count, required * count // 100)
            summary[name] = {
                "count": count,
                "covered": covered,
                "percent": covered * 100.0 / count if count else 0,
            }
        critical_files.append({"filename": path, "summary": summary})
    return json.dumps(
        {
            "type": "llvm.coverage.json.export",
            "version": "2.0.1",
            "data": [{"totals": totals, "files": critical_files}],
        }
    ).encode()


def write_exception_registry(path: Path, rows: list[dict[str, object]]) -> None:
    path.write_text(json.dumps({"schema": 1, "exceptions": rows}), encoding="utf-8")


class CoverageEvidenceTests(unittest.TestCase):
    def test_exact_thresholds_pass_from_recomputed_integer_counters(self) -> None:
        verdict = gate.parse_llvm_coverage(llvm_payload())
        self.assertTrue(verdict.passed)
        self.assertEqual(verdict.metrics["lines"].percent, 85.0)
        self.assertEqual(verdict.metrics["branches"].percent, 80.0)
        self.assertEqual(len(verdict.artifact_sha256), 64)

    def test_each_subthreshold_metric_is_named_and_fails(self) -> None:
        verdict = gate.parse_llvm_coverage(
            llvm_payload(lines=(100, 84), regions=(100, 84), functions=(100, 79), branches=(100, 79))
        )
        self.assertFalse(verdict.passed)
        self.assertEqual(len(verdict.failures), 4)
        for metric in ("lines", "regions", "functions", "branches"):
            self.assertTrue(any(message.startswith(metric) for message in verdict.failures), verdict.failures)

    def test_critical_domains_are_complete_stricter_and_cannot_be_omitted(self) -> None:
        verdict = gate.parse_llvm_coverage(llvm_payload())
        self.assertEqual(set(verdict.critical_domains), set(gate.CRITICAL_COVERAGE_DOMAINS))
        self.assertTrue(all(domain.passed for domain in verdict.critical_domains.values()))
        self.assertTrue(
            all(
                domain.metrics["lines"].required_percent == 95.0
                and domain.metrics["branches"].required_percent == 90.0
                for domain in verdict.critical_domains.values()
            )
        )

        document = json.loads(llvm_payload())
        document["data"][0]["files"] = [
            row
            for row in document["data"][0]["files"]
            if row["filename"] != "src-tauri/src/review_campaign.rs"
        ]
        omitted = gate.parse_llvm_coverage(json.dumps(document).encode())
        self.assertFalse(omitted.passed)
        self.assertTrue(
            any(
                "critical review coverage pattern has no LLVM file evidence" in row
                for row in omitted.failures
            )
        )

    def test_critical_domain_subthreshold_and_duplicate_file_identity_fail(self) -> None:
        document = json.loads(llvm_payload())
        review = next(
            row
            for row in document["data"][0]["files"]
            if row["filename"] == "src-tauri/src/review_campaign.rs"
        )
        review["summary"]["lines"] = {"count": 10000, "covered": 1, "percent": 0.01}
        verdict = gate.parse_llvm_coverage(json.dumps(document).encode())
        self.assertFalse(verdict.passed)
        self.assertTrue(any("critical review lines coverage" in row for row in verdict.failures))

        document = json.loads(llvm_payload())
        duplicate = dict(document["data"][0]["files"][0])
        duplicate["filename"] = duplicate["filename"].replace("/", "\\")
        document["data"][0]["files"].append(duplicate)
        with self.assertRaisesRegex(gate.GateError, "repeats critical-source identity"):
            gate.parse_llvm_coverage(json.dumps(document).encode())

    def test_missing_and_zero_denominator_branch_evidence_fail_closed(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "missing the 'branches'"):
            gate.parse_llvm_coverage(llvm_payload(branches=None))
        with self.assertRaisesRegex(gate.GateError, "zero denominator.*unproven"):
            gate.parse_llvm_coverage(llvm_payload(branches=(0, 0)))

    def test_claimed_percent_cannot_disagree_with_authoritative_counters(self) -> None:
        document = json.loads(llvm_payload())
        document["data"][0]["totals"]["lines"]["percent"] = 99.0
        with self.assertRaisesRegex(gate.GateError, "disagrees with its counters"):
            gate.parse_llvm_coverage(json.dumps(document).encode())

    def test_boolean_impossible_and_nonfinite_counters_are_not_coerced(self) -> None:
        for bad in (True, -1, 101):
            document = json.loads(llvm_payload())
            document["data"][0]["totals"]["lines"]["covered"] = bad
            with self.assertRaises(gate.GateError, msg=f"bad covered counter {bad!r} was accepted"):
                gate.parse_llvm_coverage(json.dumps(document).encode())

    def test_multiple_data_sets_are_not_silently_reduced_or_cherry_picked(self) -> None:
        document = json.loads(llvm_payload())
        document["data"].append(document["data"][0])
        with self.assertRaisesRegex(gate.GateError, "exactly one unambiguous"):
            gate.parse_llvm_coverage(json.dumps(document).encode())

    def test_command_is_argument_array_with_every_required_measurement_flag(self) -> None:
        command = gate._coverage_command(Path("proof.json"))
        self.assertEqual(command[0], "cargo")
        self.assertEqual(command[1], "+nightly-2026-07-11")
        self.assertNotIn("+nightly", command)
        for flag in gate.REQUIRED_COVERAGE_FLAGS:
            self.assertIn(flag, command)
        self.assertIn("--branch", command)
        self.assertNotIn("--summary-only", command)

    def test_toolchain_contract_rejects_rolling_nightly_and_unknown_fields(self) -> None:
        document = json.loads(gate.DEFAULT_COVERAGE_TOOLCHAIN_FILE.read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as directory:
            contract = Path(directory) / "toolchain.json"
            for mutation, expected in (
                ({"toolchain": "nightly"}, "toolchain.*malformed"),
                ({"surprise": True}, "missing or unknown fields"),
            ):
                contract.write_text(json.dumps(document | mutation), encoding="utf-8")
                with self.subTest(mutation=mutation), self.assertRaisesRegex(gate.GateError, expected):
                    gate.load_coverage_toolchain_contract(contract)

    def test_probe_identity_mismatch_refuses_before_measurement(self) -> None:
        identity = gate.expected_coverage_toolchain_identity()
        rustc = "\n".join(
            (
                identity.rustc_version,
                "binary: rustc",
                f"commit-hash: {identity.rustc_commit_hash}",
                f"commit-date: {identity.rustc_commit_date}",
                f"host: {identity.host}",
                "release: 1.99.0-nightly",
                f"LLVM version: {identity.llvm_version}",
            )
        )
        cargo = "\n".join(
            (
                identity.cargo_version,
                "release: 1.99.0-nightly",
                f"commit-hash: {identity.cargo_commit_hash}",
                f"commit-date: {identity.cargo_commit_date}",
                f"host: {identity.host}",
            )
        )
        with mock.patch.object(
            gate,
            "_probe",
            side_effect=[rustc, cargo, "cargo-llvm-cov 0.0.0"],
        ):
            with self.assertRaisesRegex(gate.GateError, "does not match its exact contract"):
                gate.verify_coverage_toolchain()

    def test_failed_fresh_measurement_cannot_reuse_a_stale_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "coverage.json"
            artifact.write_bytes(llvm_payload())
            completed = subprocess.CompletedProcess(["cargo"], 2)
            with mock.patch.object(
                gate,
                "verify_coverage_toolchain",
                return_value=gate.expected_coverage_toolchain_identity(),
            ), mock.patch.object(gate.subprocess, "run", return_value=completed):
                with self.assertRaisesRegex(gate.GateError, "cannot fall back"):
                    gate.run_coverage(output_path=artifact, timeout_seconds=1)
            self.assertFalse(artifact.exists(), "stale coverage survived the failed measurement")

    def test_successful_runner_validates_the_newly_written_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "coverage.json"

            def fake_run(command: list[str], **_: object) -> subprocess.CompletedProcess[list[str]]:
                self.assertIn("--branch", command)
                artifact.write_bytes(llvm_payload())
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(
                gate,
                "verify_coverage_toolchain",
                return_value=gate.expected_coverage_toolchain_identity(),
            ), mock.patch.object(gate.subprocess, "run", side_effect=fake_run):
                execution = gate.run_coverage(output_path=artifact, timeout_seconds=1)
            self.assertTrue(execution.verdict.passed)
            self.assertEqual(execution.toolchain.toolchain, "nightly-2026-07-11")


class RustSourceMeasurementTests(unittest.TestCase):
    def test_cfg_test_items_are_removed_but_cfg_tokens_in_literals_and_comments_are_not(self) -> None:
        source = """// #[cfg(test)] fn fake() {}
const TEXT: &str = r###"#[cfg(test)] { not code }"###;
fn shipped() {
    println!("#[cfg(test)] {{ still a string }}");
}
#[cfg(test)]
fn only_test() {
    let nested = if true { Some(1) } else { None };
    assert_eq!(nested, Some(1));
}
fn after_test() {}
"""
        physical, test_only, production = gate.production_line_count(source)
        self.assertEqual(physical, 11)
        self.assertEqual(test_only, 5)
        self.assertEqual(production, 6)

    def test_nested_block_comments_and_multiline_strings_do_not_confuse_brace_matching(self) -> None:
        source = '''/* outer { /* nested } */ done */
fn real() { let text = "a multiline
string with }"; }
#[cfg(test)] #[allow(dead_code)]
mod tests { const RAW: &str = r"}"; fn nested() { if true { } } }
fn tail() {}
'''
        excluded = gate.cfg_test_line_numbers(source)
        self.assertEqual(excluded, {4, 5})
        self.assertNotIn(6, excluded)

    def test_cfg_test_semicolon_item_is_bounded_without_eating_following_production(self) -> None:
        source = "#[cfg(test)]\nuse crate::fake;\nfn real() {}\n"
        self.assertEqual(gate.cfg_test_line_numbers(source), {1, 2})
        self.assertEqual(gate.production_line_count(source)[2], 1)


class ArchitecturePolicyTests(unittest.TestCase):
    def make_app(self, directory: str, modules: dict[str, str]) -> Path:
        app = Path(directory)
        root = app / "src-tauri" / "src"
        root.mkdir(parents=True)
        for relative, source in modules.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source, encoding="utf-8")
        return app

    def test_1999_lines_pass_and_2000_lines_fail_literal_below_limit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = self.make_app(directory, {"ok.rs": "// line\n" * 1999, "bad.rs": "// line\n" * 2000})
            registry = app / "exceptions.json"
            write_exception_registry(registry, [])
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertFalse(verdict.passed)
            self.assertEqual(len(verdict.failures), 1)
            self.assertIn("bad.rs: 2000 production lines", verdict.failures[0])

    def test_exact_hash_bound_immutable_history_exception_can_cover_only_hard_ceiling_module(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = "// immutable history\n" * 2_500
            app = self.make_app(directory, {"history.rs": source})
            digest = gate._sha256_bytes((app / "src-tauri" / "src" / "history.rs").read_bytes())
            registry = app / "exceptions.json"
            write_exception_registry(
                registry,
                [
                    {
                        "path": "src-tauri/src/history.rs",
                        "kind": "immutable-history",
                        "sha256": digest,
                        "max_production_lines": 2500,
                        "reason": "Locked append-only historical catalog whose byte identity is independently verified.",
                        "basis": "Approved architecture contract for immutable historical catalogs.",
                    }
                ],
            )
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertTrue(verdict.passed, verdict.failures)
            self.assertEqual(verdict.measurements[0].exception, "immutable-history")

    def test_stale_hash_does_not_exempt_an_oversized_module(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = "// immutable history\n" * 2_500
            app = self.make_app(directory, {"history.rs": source})
            registry = app / "exceptions.json"
            write_exception_registry(
                registry,
                [
                    {
                        "path": "src-tauri/src/history.rs",
                        "kind": "immutable-history",
                        "sha256": "0" * 64,
                        "max_production_lines": 2500,
                        "reason": "Locked append-only historical catalog whose byte identity is independently verified.",
                        "basis": "Approved architecture contract for immutable historical catalogs.",
                    }
                ],
            )
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertFalse(verdict.passed)
            self.assertTrue(any("hash is stale" in message for message in verdict.failures))
            self.assertTrue(any("violates the below-2000" in message for message in verdict.failures))

    def test_exception_below_hard_ceiling_is_rejected_instead_of_becoming_general_waiver(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = "// ordinary oversized module\n" * 2_100
            app = self.make_app(directory, {"ordinary.rs": source})
            registry = app / "exceptions.json"
            write_exception_registry(
                registry,
                [
                    {
                        "path": "src-tauri/src/ordinary.rs",
                        "kind": "immutable-history",
                        "sha256": gate._sha256_bytes(source.encode()),
                        "max_production_lines": 2500,
                        "reason": "This text is deliberately long enough to pass the substantive-reason parser.",
                        "basis": "This basis exists only to exercise hard ceiling enforcement.",
                    }
                ],
            )
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertFalse(verdict.passed)
            self.assertTrue(any("not allowed below" in message for message in verdict.failures))

    def test_wildcard_unknown_kind_unknown_fields_and_unused_entries_fail_closed(self) -> None:
        base = {
            "path": "src-tauri/src/history.rs",
            "kind": "immutable-history",
            "sha256": "0" * 64,
            "max_production_lines": 2500,
            "reason": "A sufficiently detailed reason for an immutable historical module exception.",
            "basis": "A sufficiently detailed and reviewable policy basis.",
        }
        with tempfile.TemporaryDirectory() as directory:
            registry = Path(directory) / "exceptions.json"
            for mutation, expected in (
                ({"path": "src-tauri/src/*.rs"}, "without wildcards"),
                ({"kind": "legacy"}, "unsupported kind"),
                ({"surprise": True}, "missing or unknown fields"),
            ):
                row = base | mutation
                write_exception_registry(registry, [row])
                with self.assertRaisesRegex(gate.GateError, expected):
                    gate.load_module_exceptions(registry)

            app = self.make_app(directory, {"lib.rs": "fn real() {}\n"})
            write_exception_registry(registry, [base])
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertTrue(any("unused module exception" in message for message in verdict.failures))

    def test_test_only_companion_modules_are_not_misreported_as_production(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = self.make_app(
                directory,
                {
                    "lib.rs": '#[cfg(test)]\n#[path = "db_tests.rs"]\nmod tests;\nfn shipped() {}\n',
                    "db_tests.rs": "#[test]\nfn test() {}\n" * 3_000,
                },
            )
            registry = app / "exceptions.json"
            write_exception_registry(registry, [])
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertTrue(verdict.passed)
            self.assertEqual([row.path for row in verdict.measurements], ["src-tauri/src/lib.rs"])

    def test_testish_filename_without_cfg_test_declaration_cannot_hide_shipped_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = self.make_app(
                directory,
                {"lib.rs": "fn shipped() {}\n", "important_tests.rs": "// shipped despite its name\n" * 2_500},
            )
            registry = app / "exceptions.json"
            write_exception_registry(registry, [])
            verdict = gate.evaluate_architecture(app_root=app, exception_file=registry)
            self.assertFalse(verdict.passed)
            self.assertTrue(any("important_tests.rs: 2500" in message for message in verdict.failures))


if __name__ == "__main__":
    unittest.main(verbosity=2)
