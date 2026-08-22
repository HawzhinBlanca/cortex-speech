import importlib.util
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_database_integrity.py")
VERIFY_10 = SCRIPT.parents[2] / "scripts" / "verify_10.py"
MIGRATIONS = SCRIPT.parents[1] / "src-tauri" / "src" / "migrations" / "mod.rs"
sys.path.insert(0, str(SCRIPT.parent))

from review_pilot_hidden_contract import HIDDEN_SCHEMA_SQL  # noqa: E402
from review_pilot_hidden_contract import audit_hidden_schema  # noqa: E402


def load_gate():
    spec = importlib.util.spec_from_file_location("database_integrity_gate", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_migrations() -> list[tuple[int, str]]:
    spec = importlib.util.spec_from_file_location("database_integrity_fixture_source", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.source_migrations(MIGRATIONS)


class DatabaseIntegrityGateTests(unittest.TestCase):
    def test_shared_hidden_key_contract_matches_the_actual_v59_rust_migration(self):
        source = MIGRATIONS.read_text(encoding="utf-8")
        match = re.search(
            r'version:\s*59,.*?up_sql:\s*"(?P<sql>.*?)",\s*// Once an assignment exists',
            source,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(match, "could not extract migration 59 SQL")
        connection = sqlite3.connect(":memory:")
        connection.execute("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT)")
        # The hidden-key table is introduced by v59, while the operational contract now requires
        # the complete v60 schema.  Give this isolated schema fixture the current frontier without
        # pretending the hidden table moved migrations.
        connection.executemany(
            "INSERT INTO schema_migrations VALUES(?, 'fixture')",
            [(59,), (60,)],
        )
        connection.executescript(match.group("sql"))
        _evidence, errors = audit_hidden_schema(connection)
        connection.close()
        self.assertEqual(errors, [], errors)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.db = self.root / "cortex-speech.db"
        connection = sqlite3.connect(self.db)
        connection.executescript(
            """
            PRAGMA foreign_keys=ON;
            CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT);
            CREATE TABLE parent(id TEXT PRIMARY KEY);
            CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id TEXT NOT NULL REFERENCES parent(id));
            CREATE TABLE orphan_segment_hypotheses_archive_v58 (
                original_rowid INTEGER NOT NULL UNIQUE,
                segment_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                transcript TEXT NOT NULL,
                confidence REAL,
                created_at TEXT NOT NULL,
                model_version_id TEXT NOT NULL,
                source_table TEXT NOT NULL,
                archive_reason TEXT NOT NULL,
                archive_migration_version INTEGER NOT NULL,
                archived_at TEXT NOT NULL,
                PRIMARY KEY(segment_id, model_id)
            );
            CREATE TABLE orphan_loop0_shadow_log_archive_v58 (
                id INTEGER PRIMARY KEY,
                segment_id TEXT NOT NULL,
                memory_fired BOOLEAN,
                created_at TEXT,
                source_table TEXT NOT NULL,
                archive_reason TEXT NOT NULL,
                archive_migration_version INTEGER NOT NULL,
                archived_at TEXT NOT NULL
            );
            CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert
            BEFORE INSERT ON orphan_segment_hypotheses_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_update
            BEFORE UPDATE ON orphan_segment_hypotheses_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_delete
            BEFORE DELETE ON orphan_segment_hypotheses_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_insert
            BEFORE INSERT ON orphan_loop0_shadow_log_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_update
            BEFORE UPDATE ON orphan_loop0_shadow_log_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_delete
            BEFORE DELETE ON orphan_loop0_shadow_log_archive_v58
            BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
            INSERT INTO parent VALUES('p1');
            INSERT INTO child VALUES(1, 'p1');
            """
        )
        connection.executescript(HIDDEN_SCHEMA_SQL)
        connection.executemany("INSERT INTO schema_migrations VALUES(?, ?)", source_migrations())
        connection.commit()
        connection.close()

    def tearDown(self):
        self.temp.cleanup()

    def run_gate(self, db: Path | None = None):
        completed = subprocess.run(
            [sys.executable, str(SCRIPT), "--db", str(db or self.db)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        return completed.returncode, json.loads(completed.stdout)

    def test_clean_database_passes_all_three_checks(self):
        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertEqual(report["quickCheck"], ["ok"])
        self.assertEqual(report["integrityCheck"], ["ok"])
        self.assertEqual(report["foreignKeyViolations"], 0)
        self.assertEqual(report["schemaVersion"], 60)
        self.assertEqual(report["migrationHistoryEntries"], 60)
        self.assertEqual(report["v58HypothesisArchiveRows"], 0)
        self.assertEqual(report["v58Loop0ArchiveRows"], 0)
        self.assertEqual(report["v58ImmutableTriggers"], 6)
        self.assertEqual(report["pilotHiddenRows"], 0)
        self.assertEqual(len(report["pilotHiddenTriggers"]), 4)

    def test_v59_hidden_table_and_triggers_must_match_exactly(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_pilot_hidden_keys_immutable_update")
        connection.execute(
            "CREATE TRIGGER review_pilot_hidden_keys_immutable_update "
            "BEFORE UPDATE ON review_pilot_hidden_keys "
            "BEGIN SELECT RAISE(ABORT, 'different message'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("does not exactly match" in error for error in report["errors"]), report)

    def test_v59_historical_quota_overage_is_red_even_if_trigger_is_restored(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert")
        connection.executemany(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 0, ?, ?)",
            [
                ("a" * 64, "Hawzhin", "hidden-h1"),
                ("a" * 64, "Hawzhin", "hidden-h2"),
                ("a" * 64, "Hawzhin", "hidden-h3"),
                ("a" * 64, "Pavel", "hidden-p1"),
                ("a" * 64, "Rubar", "hidden-r1"),
            ],
        )
        connection.execute(
            """CREATE TRIGGER review_pilot_hidden_keys_quota_insert
               BEFORE INSERT ON review_pilot_hidden_keys
               WHEN NOT EXISTS (
                   SELECT 1 FROM review_pilot_hidden_keys
                    WHERE policy_sha256 = NEW.policy_sha256
                      AND after_review_event_id = NEW.after_review_event_id
                      AND reviewer = NEW.reviewer
                      AND segment_id = NEW.segment_id
               )
               AND (
                   (SELECT COUNT(*) FROM review_pilot_hidden_keys
                     WHERE policy_sha256 = NEW.policy_sha256
                       AND after_review_event_id = NEW.after_review_event_id
                       AND reviewer = NEW.reviewer) >= 2
                   OR
                   (SELECT COUNT(*) FROM review_pilot_hidden_keys
                     WHERE policy_sha256 = NEW.policy_sha256
                       AND after_review_event_id = NEW.after_review_event_id) >= 4
               )
               BEGIN SELECT RAISE(ABORT, 'controlled review pilot hidden-key quota exceeded'); END"""
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertEqual(report["pilotHiddenReviewerOverages"], 1)
        self.assertEqual(report["pilotHiddenNamespaceOverages"], 1)
        self.assertTrue(any("exceeds max 2" in error for error in report["errors"]), report)
        self.assertTrue(any("exceeds max 4" in error for error in report["errors"]), report)

    def test_v58_missing_trigger_or_asymmetric_archive_is_red(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_delete")
        connection.execute("DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert")
        connection.execute(
            "INSERT INTO orphan_segment_hypotheses_archive_v58 VALUES "
            "(3000, '00000000-0000-4000-8000-000000000000', 'omniasr-7b-legacy-c348ade8a816', "
            "'x', NULL, '2026-08-21', 'omniasr-7b-legacy-c348ade8a816', 'segment_hypotheses', "
            "'missing speech_segments parent', 58, '2026-08-21')"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("trigger(s) missing" in error for error in report["errors"]), report)
        self.assertTrue(any("archive counts" in error for error in report["errors"]), report)

    def test_v58_exact_custom_cohort_passes_and_wrong_digest_or_metadata_is_red(self):
        ids = [
            "00000000-0000-4000-8000-000000000000",
            "00000000-0000-4000-8000-000000000001",
        ]
        digest = hashlib.sha256("".join(f"{item}\n" for item in ids).encode()).hexdigest()
        full_tuple_hasher = hashlib.sha256()
        model = "omniasr-7b-legacy-c348ade8a816"
        for index, segment_id in enumerate(ids):
            rowid = 3000 + index
            source_pair = [
                [rowid, segment_id, model, "x", None, "2026-08-21", model],
                [rowid - 2555, segment_id, 0, "2026-08-21"],
            ]
            full_tuple_hasher.update(
                (json.dumps(source_pair, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
            )
        full_tuple_digest = full_tuple_hasher.hexdigest()
        connection = sqlite3.connect(self.db)
        connection.executescript(
            "DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert;"
            "DROP TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_insert;"
        )
        for index, segment_id in enumerate(ids):
            rowid = 3000 + index
            connection.execute(
                "INSERT INTO orphan_segment_hypotheses_archive_v58 VALUES "
                "(?, ?, 'omniasr-7b-legacy-c348ade8a816', 'x', NULL, '2026-08-21', "
                "'omniasr-7b-legacy-c348ade8a816', 'segment_hypotheses', "
                "'missing speech_segments parent', 58, '2026-08-21')",
                (rowid, segment_id),
            )
            connection.execute(
                "INSERT INTO orphan_loop0_shadow_log_archive_v58 VALUES "
                "(?, ?, 0, '2026-08-21', 'loop0_shadow_log', 'missing speech_segments parent', 58, '2026-08-21')",
                (rowid - 2555, segment_id),
            )
        connection.executescript(
            "CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert "
            "BEFORE INSERT ON orphan_segment_hypotheses_archive_v58 "
            "BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;"
            "CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_insert "
            "BEFORE INSERT ON orphan_loop0_shadow_log_archive_v58 "
            "BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;"
        )
        connection.commit()
        connection.close()

        gate = load_gate()
        report = gate.audit(
            self.db,
            require_production_v58_repair=True,
            expected_v58_rows=2,
            expected_v58_digest=digest,
            expected_v58_full_tuple_digest=full_tuple_digest,
        )
        self.assertTrue(report["ok"], report)
        wrong_digest = gate.audit(
            self.db,
            require_production_v58_repair=True,
            expected_v58_rows=2,
            expected_v58_digest="0" * 64,
            expected_v58_full_tuple_digest=full_tuple_digest,
        )
        self.assertFalse(wrong_digest["ok"], wrong_digest)
        self.assertTrue(any("digest" in error for error in wrong_digest["errors"]), wrong_digest)

        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_update")
        connection.execute(
            "UPDATE orphan_segment_hypotheses_archive_v58 SET model_id = 'wrong' WHERE segment_id = ?",
            (ids[0],),
        )
        connection.execute(
            "CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_update "
            "BEFORE UPDATE ON orphan_segment_hypotheses_archive_v58 "
            "BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END"
        )
        connection.commit()
        connection.close()
        bad_shape = gate.audit(
            self.db,
            require_production_v58_repair=True,
            expected_v58_rows=2,
            expected_v58_digest=digest,
            expected_v58_full_tuple_digest=full_tuple_digest,
        )
        self.assertFalse(bad_shape["ok"], bad_shape)
        self.assertTrue(any("provenance/shape" in error for error in bad_shape["errors"]), bad_shape)

    def test_orphan_is_grouped_and_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("PRAGMA foreign_keys=OFF")
        connection.execute("INSERT INTO child VALUES(2, 'missing')")
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertEqual(report["foreignKeyViolations"], 1)
        self.assertEqual(report["foreignKeyViolationsByTable"], {"child": 1})
        self.assertTrue(any("foreign_key_check" in error for error in report["errors"]), report)

    def test_clean_but_stale_schema_is_red(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DELETE FROM schema_migrations WHERE version = 60")
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertEqual(report["schemaVersion"], 59)
        self.assertEqual(report["requiredSchemaVersion"], 60)
        self.assertTrue(any("missing=[60]" in error for error in report["errors"]), report)

    def test_missing_middle_history_or_wrong_description_is_red(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DELETE FROM schema_migrations WHERE version = 23")
        connection.execute("UPDATE schema_migrations SET description = 'tampered' WHERE version = 31")
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertEqual(report["schemaVersion"], 60, "MAX alone would falsely green this fixture")
        self.assertTrue(any("missing=[23]" in error for error in report["errors"]), report)
        self.assertTrue(any("descriptionMismatch=[31]" in error for error in report["errors"]), report)

    def test_missing_database_is_red_and_is_not_created(self):
        missing = self.root / "missing.db"
        code, report = self.run_gate(missing)
        self.assertEqual(code, 1, report)
        self.assertFalse(missing.exists())
        self.assertIn("does not exist", report["errors"][0])

    def test_inherited_cortex_db_cannot_redirect_the_production_default(self):
        spec = importlib.util.spec_from_file_location("database_integrity_default_path", SCRIPT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        previous = os.environ.get("CORTEX_DB")
        try:
            os.environ["CORTEX_DB"] = str(self.db)
            expected_base = Path(os.environ["APPDATA"]) if os.environ.get("APPDATA") else Path.home() / ".local" / "share"
            self.assertEqual(module.default_db_path(), expected_base / "cortex-speech" / "cortex-speech.db")
            self.assertNotEqual(module.default_db_path(), self.db)
        finally:
            if previous is None:
                os.environ.pop("CORTEX_DB", None)
            else:
                os.environ["CORTEX_DB"] = previous

    def test_verify_10_runs_global_integrity_without_a_skip_probe(self):
        spec = importlib.util.spec_from_file_location("verify_10_database_integrity", VERIFY_10)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        verify = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = verify
        spec.loader.exec_module(verify)

        matches = [entry for entry in verify.GATES if entry[0] == "database-integrity-live"]
        self.assertEqual(len(matches), 1)
        _name, tier, kind, payload, cwd, probe, _charter = matches[0]
        self.assertEqual((tier, kind, cwd), (2, "cmd", verify.APP))
        self.assertIsNone(probe, "missing or corrupt live data must be RED, never skipped")
        self.assertIn(str(SCRIPT), payload)
        self.assertNotIn("--db", payload)
        self.assertIn("--require-production-v58-repair", payload)


if __name__ == "__main__":
    unittest.main()
