#!/usr/bin/env python3
"""Fail-closed regressions for the owner-critical mutation campaign producer."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_owner_mutation_campaign as campaign


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def mutant(identifier: str, status: str) -> dict[str, object]:
    return {
        "id": identifier,
        "mutatorName": "ConditionalExpression",
        "replacement": "false",
        "status": status,
        "location": {
            "start": {"line": 1, "column": 0},
            "end": {"line": 1, "column": 1},
        },
    }


class FrontendRawEvidenceTests(unittest.TestCase):
    def make_frontend(self, root: Path, statuses: dict[str, list[str]]) -> dict[str, object]:
        files: dict[str, object] = {}
        all_mutants: list[dict[str, object]] = []
        sequence = 0
        for source, source_statuses in statuses.items():
            source_path = root / source
            source_path.parent.mkdir(parents=True, exist_ok=True)
            source_path.write_text("export const value = true;", encoding="utf-8")
            rows = []
            for status in source_statuses:
                row = mutant(str(sequence), status)
                rows.append(row)
                all_mutants.append(row)
                sequence += 1
            files[source] = {"language": "typescript", "source": "export const value = true;", "mutants": rows}
        report = {
            "schemaVersion": "1.0",
            "files": files,
            "thresholds": {"high": 100, "low": 80, "break": 80},
            "testFiles": {},
            "projectRoot": str(root),
            "config": {},
            "framework": {"name": "StrykerJS", "version": "10.0.0"},
        }
        write_json(root / campaign.FRONTEND_REPORT, report)
        events = root / campaign.FRONTEND_EVENTS
        test_files = [
            "tests/lib/audioMachine.test.ts",
            "src/lib/reviewCommitOperation.test.ts",
            "src/lib/reviewCommitResult.test.ts",
        ]
        for test_file in test_files:
            path = root / test_file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("// fixture\n", encoding="utf-8")
        write_json(
            events / "00000-onDryRunCompleted.json",
            {
                "result": {
                    "status": "complete",
                    "tests": [
                        {
                            "id": f"{test_file}#fixture",
                            "fileName": str((root / test_file).resolve()),
                            "status": 0,
                        }
                        for test_file in test_files
                    ],
                }
            },
        )
        write_json(
            events / "00001-onMutationTestingPlanReady.json",
            {"mutantPlans": [{"plan": "Run", "mutant": {"id": row["id"]}} for row in all_mutants]},
        )
        for index, row in enumerate(all_mutants, start=2):
            write_json(events / f"{index:05d}-onMutantTested.json", row)
        write_json(
            events / f"{len(all_mutants) + 2:05d}-onMutationTestReportReady.json",
            report,
        )
        (root / campaign.FRONTEND_LOG).write_text("official trace\n", encoding="utf-8")
        return report

    def valid_statuses(self) -> dict[str, list[str]]:
        return {
            "src/lib/audioMachine.ts": ["Killed"] * 8 + ["Survived"] * 2,
            "src/lib/reviewCommitOperation.ts": ["Killed"] * 4 + ["Survived"],
            "src/lib/reviewCommitResult.ts": ["Killed"],
        }

    def test_official_report_and_complete_event_stream_pass_at_exact_domain_floors(self) -> None:
        contract = campaign._load_contract()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_frontend(root, self.valid_statuses())
            result = campaign._validate_frontend_raw(root, contract, 80.0)
            self.assertEqual(result["mutants"], 16)
            self.assertEqual(result["domains"]["audio-state-machine"]["scorePercent"], 80.0)

    def test_missing_outcome_substituted_report_and_subthreshold_domain_fail(self) -> None:
        contract = campaign._load_contract()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_frontend(root, self.valid_statuses())
            tested = sorted((root / campaign.FRONTEND_EVENTS).glob("*-onMutantTested.json"))
            tested[0].unlink()
            with self.assertRaisesRegex(campaign.CampaignError, "one raw outcome"):
                campaign._validate_frontend_raw(root, contract, 80.0)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = self.make_frontend(root, self.valid_statuses())
            report["files"]["src/lib/audioMachine.ts"]["mutants"][-1]["status"] = "Killed"
            write_json(root / campaign.FRONTEND_REPORT, report)
            with self.assertRaisesRegex(campaign.CampaignError, "not an exact projection"):
                campaign._validate_frontend_raw(root, contract, 80.0)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            low = self.valid_statuses()
            low["src/lib/audioMachine.ts"] = ["Killed"] * 7 + ["Survived"] * 3
            self.make_frontend(root, low)
            with self.assertRaisesRegex(campaign.CampaignError, "audio-state-machine is below 80%"):
                campaign._validate_frontend_raw(root, contract, 80.0)

    def test_boolean_threshold_and_untracked_event_fail_closed(self) -> None:
        contract = campaign._load_contract()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = self.make_frontend(root, self.valid_statuses())
            report["thresholds"]["high"] = True
            write_json(root / campaign.FRONTEND_REPORT, report)
            with self.assertRaisesRegex(campaign.CampaignError, "exact floor"):
                campaign._validate_frontend_raw(root, contract, 80.0)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_frontend(root, self.valid_statuses())
            write_json(root / campaign.FRONTEND_EVENTS / "99999-untracked.json", {})
            with self.assertRaisesRegex(campaign.CampaignError, "one raw outcome"):
                campaign._validate_frontend_raw(root, contract, 80.0)


class BackendRawEvidenceTests(unittest.TestCase):
    def make_app(self, root: Path) -> tuple[Path, list[dict[str, object]]]:
        app = root / "app"
        for source in ("src-tauri/src/review.rs", "src-tauri/src/restore.rs"):
            path = app / source
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fn value() -> bool { true }\n", encoding="utf-8")
        inventory = [
            {
                "file": "src/review.rs",
                "function": None,
                "genre": "BooleanLiteral",
                "name": "review true to false",
                "package": "fixture",
                "replacement": "false",
                "span": {"start": {"line": 1, "column": 21}, "end": {"line": 1, "column": 25}},
            },
            {
                "file": "src/restore.rs",
                "function": None,
                "genre": "BooleanLiteral",
                "name": "restore true to false",
                "package": "fixture",
                "replacement": "false",
                "span": {"start": {"line": 1, "column": 21}, "end": {"line": 1, "column": 25}},
            },
        ]
        return app, inventory

    def outcome_row(self, row: dict[str, object], summary: str) -> dict[str, object]:
        return {"scenario": {"Mutant": copy.deepcopy(row)}, "summary": summary}

    def test_raw_inventory_and_outcomes_are_complete_and_domain_scored(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app, inventory = self.make_app(root)
            inventory_path = root / "inventory.json"
            outcomes_path = root / "outcomes.json"
            write_json(inventory_path, inventory)
            write_json(
                outcomes_path,
                {
                    "cargo_mutants_version": "27.1.0",
                    "outcomes": [
                        {"scenario": "Baseline", "summary": "Success"},
                        self.outcome_row(inventory[0], "CaughtMutant"),
                        self.outcome_row(inventory[1], "CaughtMutant"),
                    ],
                },
            )
            result = campaign._validate_backend_raw(
                inventory_path=inventory_path,
                native_inventory_path=inventory_path,
                outcomes_path=outcomes_path,
                app=app,
                by_domain={
                    "review": ["src-tauri/src/review.rs"],
                    "restore": ["src-tauri/src/restore.rs"],
                },
                minimum=90,
            )
            self.assertEqual(result["mutants"], 2)
            self.assertEqual(result["killed"], 2)

            document = json.loads(outcomes_path.read_text(encoding="utf-8"))
            document["outcomes"].pop()
            write_json(outcomes_path, document)
            with self.assertRaisesRegex(campaign.CampaignError, "every discovered mutant"):
                campaign._validate_backend_raw(
                    inventory_path=inventory_path,
                    native_inventory_path=inventory_path,
                    outcomes_path=outcomes_path,
                    app=app,
                    by_domain={
                        "review": ["src-tauri/src/review.rs"],
                        "restore": ["src-tauri/src/restore.rs"],
                    },
                    minimum=90,
                )


class RawAuthorityBundleTests(unittest.TestCase):
    def test_length_delimited_bundle_round_trips_and_rejects_tampering(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.json"
            second = root / "second.log"
            first.write_bytes(b'{"native":true}\n')
            second.write_bytes(b"command output\n")
            bundle_path = root / campaign.RAW_BUNDLE_NAME
            bundle = campaign._write_raw_bundle(
                bundle_path,
                [("native/second.log", second), ("native/first.json", first)],
            )
            manifest = {"bundle": bundle}
            extracted = root / "extracted"
            campaign._extract_raw_bundle(manifest, bundle_path, extracted)
            self.assertEqual((extracted / "native" / "first.json").read_bytes(), first.read_bytes())
            self.assertEqual((extracted / "native" / "second.log").read_bytes(), second.read_bytes())
            self.assertEqual(
                [row["path"] for row in bundle["entries"]],
                ["native/first.json", "native/second.log"],
            )

            corrupted = bytearray(bundle_path.read_bytes())
            corrupted[-1] ^= 1
            bundle_path.write_bytes(corrupted)
            with self.assertRaisesRegex(campaign.CampaignError, "do not match the manifest"):
                campaign._extract_raw_bundle(manifest, bundle_path, root / "tampered")

    def test_manifest_cannot_omit_a_bundle_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_bytes(b"raw")
            bundle_path = root / campaign.RAW_BUNDLE_NAME
            bundle = campaign._write_raw_bundle(bundle_path, [("native/source", source)])
            bundle["entries"] = []
            with self.assertRaisesRegex(campaign.CampaignError, "identity is malformed"):
                campaign._extract_raw_bundle({"bundle": bundle}, bundle_path, root / "missing")

    def test_boolean_lengths_and_schema_do_not_alias_integers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle_path = root / campaign.RAW_BUNDLE_NAME
            bundle_path.write_bytes(campaign.RAW_BUNDLE_MAGIC + b"x")
            malformed_bundle = {
                "format": campaign.RAW_BUNDLE_FORMAT,
                "sha256": campaign._sha256_file(bundle_path),
                "bytes": True,
                "entries": [{"bytes": 1, "path": "native/value", "sha256": "0" * 64}],
            }
            with self.assertRaisesRegex(campaign.CampaignError, "identity is malformed"):
                campaign._extract_raw_bundle(
                    {"bundle": malformed_bundle}, bundle_path, root / "boolean-length"
                )

            manifest = {
                "schema": True,
                "type": "OwnerMutationRawAuthorityV1",
                "runToken": "00000000-0000-4000-8000-000000000000",
                "scope": ["frontend"],
                "certificationEligible": False,
                "fullGitSha": "0" * 40,
                "checkoutStateDigest": "0" * 64,
                "contractSha256": "0" * 64,
                "campaignSha256": "0" * 64,
                "authorities": {},
                "tools": {},
                "runtime": {},
                "bundle": malformed_bundle,
            }
            write_json(root / campaign.RAW_MANIFEST_NAME, manifest)
            with self.assertRaisesRegex(campaign.CampaignError, "manifest is malformed"):
                campaign._replay_raw_authority(root)

    def test_timestamps_are_fixed_width_utc_and_future_bounded(self) -> None:
        parsed = campaign._parse_utc("2020-08-28T20:00:00Z", label="fixture")
        self.assertEqual(parsed.tzinfo, campaign.dt.timezone.utc)
        for invalid in (
            "2020-08-28T20:00:00.123Z",
            "2020-8-28T20:00:00Z",
            "2999-01-01T00:00:00Z",
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(campaign.CampaignError):
                    campaign._parse_utc(invalid, label="fixture")

    def test_logged_command_rejects_an_unapproved_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(campaign.CampaignError, "returned 7"):
                campaign._run_logged(
                    [sys.executable, "-c", "raise SystemExit(7)"],
                    cwd=root,
                    log_path=root / "command.log",
                    logical_log_path="command.log",
                )
            self.assertTrue((root / "command.log").is_file())


if __name__ == "__main__":
    unittest.main(verbosity=2)
