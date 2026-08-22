import importlib.util
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_review_compensation_readiness.py")
VERIFY_10 = SCRIPT.parents[2] / "scripts" / "verify_10.py"


def load_gate_module():
    spec = importlib.util.spec_from_file_location("compensation_readiness_gate", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class CompensationReadinessGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.db = self.root / "cortex-speech.db"
        self.focus = self.root / "voice_focus.json"
        self.focus.write_text(json.dumps({"segment_ids": ["s1"]}), encoding="utf-8")
        connection = sqlite3.connect(self.db)
        connection.executescript(
            """
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, description TEXT);
            INSERT INTO schema_migrations VALUES (57, 'compensation fixture');
            CREATE TABLE speech_segments (
                id TEXT PRIMARY KEY, audio_content_hash TEXT, alignment_json TEXT, duration_ms INTEGER
            );
            CREATE TABLE review_events (
                id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, action TEXT,
                compensation_action TEXT, source TEXT, duration_ms INTEGER,
                operation_id TEXT, operation_payload_hash TEXT
            );
            CREATE TABLE review_compensation_policies (
                policy_version TEXT PRIMARY KEY, effective_after_event_id INTEGER,
                base_rate_micro_iqd_per_hour INTEGER, edit_basis_points INTEGER,
                accept_basis_points INTEGER, reject_basis_points INTEGER, skip_basis_points INTEGER
            );
            CREATE UNIQUE INDEX idx_review_events_operation_id
              ON review_events(operation_id) WHERE operation_id IS NOT NULL;
            CREATE TABLE review_compensation_ledger (
                id INTEGER PRIMARY KEY, entry_id TEXT UNIQUE, policy_version TEXT,
                review_event_id INTEGER, canonical_work_id TEXT, canonical_identity_kind TEXT,
                reviewer TEXT, segment_id TEXT, compensation_action TEXT, effective_decision TEXT,
                duration_ms INTEGER, rate_basis_points INTEGER, entitlement_micro_iqd INTEGER,
                delta_micro_iqd INTEGER, corrected_entitlement_ms INTEGER,
                delta_corrected_ms INTEGER, reverses_entry_id TEXT,
                FOREIGN KEY(policy_version) REFERENCES review_compensation_policies(policy_version),
                FOREIGN KEY(review_event_id) REFERENCES review_events(id)
            );
            CREATE TABLE review_compensation_settlements (
                settlement_id TEXT PRIMARY KEY, policy_version TEXT, reviewer TEXT,
                from_ledger_id_exclusive INTEGER, through_ledger_id_inclusive INTEGER,
                allocated_micro_iqd INTEGER, payout_reference TEXT UNIQUE,
                FOREIGN KEY(policy_version) REFERENCES review_compensation_policies(policy_version)
            );
            CREATE TRIGGER review_compensation_policy_immutable_update BEFORE UPDATE ON review_compensation_policies
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_compensation_policy_immutable_delete BEFORE DELETE ON review_compensation_policies
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_compensation_ledger_immutable_update BEFORE UPDATE ON review_compensation_ledger
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_compensation_ledger_immutable_delete BEFORE DELETE ON review_compensation_ledger
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_compensation_settlement_validate_insert BEFORE INSERT ON review_compensation_settlements
              WHEN NEW.from_ledger_id_exclusive < 0 BEGIN SELECT RAISE(ABORT, 'invalid'); END;
            CREATE TRIGGER review_compensation_settlement_immutable_update BEFORE UPDATE ON review_compensation_settlements
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_compensation_settlement_immutable_delete BEFORE DELETE ON review_compensation_settlements
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_event_operation_validate_insert BEFORE INSERT ON review_events
              WHEN (NEW.operation_id IS NULL) <> (NEW.operation_payload_hash IS NULL)
              BEGIN SELECT RAISE(ABORT, 'invalid'); END;
            CREATE TRIGGER review_event_operation_immutable_update
              BEFORE UPDATE OF operation_id, operation_payload_hash ON review_events
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            INSERT INTO speech_segments VALUES
              ('s1', 'pcm-hash', '{"source_start_ms":0,"source_end_ms":1000}', 1000);
            INSERT INTO review_compensation_policies VALUES
              ('review-iqd-v1-2026-08-21', 0, 18000000000, 10000, 1000, 1000, 0);
            INSERT INTO review_events VALUES
              (1, 's1', 'Sara', 'edit', 'edit', 'couch', 1000,
               '11111111-1111-4111-8111-111111111111',
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
            INSERT INTO review_compensation_ledger VALUES
              (1, 'entry-1', 'review-iqd-v1-2026-08-21', 1, 'work-1',
               'audio_content_hash+source_span', 'Sara', 's1', 'edit', 'edit', 1000,
               10000, 5000000, 5000000, 1000, 1000, NULL);
            """
        )
        connection.commit()
        connection.close()

    def tearDown(self):
        self.temp.cleanup()

    def run_gate(self):
        completed = subprocess.run(
            [sys.executable, str(SCRIPT), "--db", str(self.db), "--focus", str(self.focus)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        return completed.returncode, json.loads(completed.stdout)

    def test_valid_ledger_and_focus_pass(self):
        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertEqual(report["totalEarnedMicroIqd"], 5_000_000)
        self.assertEqual(report["correctedAudioMs"], 1000)

    def test_read_connection_pins_one_explicit_snapshot(self):
        gate = load_gate_module()
        connection = gate._connect_read_only(self.db)
        try:
            self.assertTrue(connection.in_transaction, "the multi-query audit must hold one read snapshot")
            self.assertEqual(connection.execute("PRAGMA query_only").fetchone()[0], 1)
        finally:
            connection.rollback()
            connection.close()

    def test_missing_event_ledger_consequence_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_delete")
        connection.execute("DELETE FROM review_compensation_ledger")
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_delete BEFORE DELETE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertIn("post-cutoff event 1 has 0 ledger entries", report["errors"])

    def test_tampered_delta_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute("UPDATE review_compensation_ledger SET delta_micro_iqd = 1")
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_update BEFORE UPDATE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("ledger delta mismatch" in error for error in report["errors"]), report)

    def test_tampered_corrected_delta_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute("UPDATE review_compensation_ledger SET delta_corrected_ms = 0")
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_update BEFORE UPDATE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("corrected delta mismatch" in error for error in report["errors"]), report)

    def test_couch_event_without_durable_operation_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_event_operation_immutable_update")
        connection.execute(
            "UPDATE review_events SET operation_id = NULL, operation_payload_hash = NULL WHERE id = 1"
        )
        connection.execute(
            "CREATE TRIGGER review_event_operation_immutable_update "
            "BEFORE UPDATE OF operation_id, operation_payload_hash ON review_events "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("operation UUID" in error for error in report["errors"]), report)

    def test_noncanonical_focus_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("UPDATE speech_segments SET audio_content_hash = NULL")
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("noncanonical pay identities" in error for error in report["errors"]), report)

    def test_boolean_alignment_coordinate_is_not_accepted_as_an_integer(self):
        connection = sqlite3.connect(self.db)
        connection.execute(
            "UPDATE speech_segments SET alignment_json = ?",
            ('{"source_start_ms":false,"source_end_ms":1000}',),
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("noncanonical pay identities" in error for error in report["errors"]), report)

    def test_duplicate_canonical_work_in_focus_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO speech_segments VALUES (?, ?, ?, ?)",
            ("s2", "pcm-hash", '{"source_start_ms":0,"source_end_ms":1000}', 1000),
        )
        connection.commit()
        connection.close()
        self.focus.write_text(json.dumps({"segment_ids": ["s1", "s2"]}), encoding="utf-8")
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("duplicate canonical pay identities" in error for error in report["errors"]), report)

    def test_verify_10_runs_live_compensation_readiness_without_a_skip_probe(self):
        """The master release verdict must not ignore a stale or corrupt live pay database."""
        spec = importlib.util.spec_from_file_location("verify_10_compensation_policy", VERIFY_10)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        verify = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = verify
        spec.loader.exec_module(verify)

        matches = [entry for entry in verify.GATES if entry[0] == "review-compensation-readiness"]
        self.assertEqual(len(matches), 1, "verify-10 must register exactly one live compensation gate")
        _name, tier, kind, payload, cwd, probe, _charter = matches[0]
        self.assertEqual((tier, kind, cwd), (2, "cmd", verify.APP))
        self.assertIsNone(probe, "missing or stale live compensation evidence must be RED, never skipped")
        self.assertIn(str(SCRIPT), payload)
        self.assertNotIn("--db", payload, "production verification must inspect the real default database")


if __name__ == "__main__":
    unittest.main()
