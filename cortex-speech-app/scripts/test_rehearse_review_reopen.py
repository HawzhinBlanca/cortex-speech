#!/usr/bin/env python3
"""Isolation guards for the production-size reopen rehearsal."""
from pathlib import Path
from contextlib import closing
import sqlite3
import tempfile
import unittest

import rehearse_review_reopen as subject


class RehearsalIsolationTests(unittest.TestCase):
    def test_snapshot_is_read_only_and_refuses_existing_destination(self):
        with tempfile.TemporaryDirectory() as raw:
            source, clone = Path(raw) / "source.db", Path(raw) / "clone.db"
            with closing(sqlite3.connect(source)) as conn:
                conn.executescript("CREATE TABLE schema_migrations(version INTEGER); INSERT INTO schema_migrations VALUES(70); "
                                   "CREATE TABLE marker(value TEXT); INSERT INTO marker VALUES('paid-history');")
            before = source.read_bytes()
            self.assertEqual(subject.copy_snapshot(source, clone), 70)
            with closing(sqlite3.connect(clone)) as conn:
                conn.execute("UPDATE marker SET value='clone-only'")
                conn.commit()
            self.assertEqual(source.read_bytes(), before)
            with self.assertRaises(ValueError):
                subject.copy_snapshot(source, clone)
            with closing(subject.read_only(source)) as reader:
                with self.assertRaises(sqlite3.OperationalError):
                    reader.execute("DELETE FROM marker")
            self.assertEqual(source.read_bytes(), before)

    def test_artifacts_cannot_overwrite_prior_evidence(self):
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "plan.json"
            subject.write_json(path, {"plan": "first"})
            before = path.read_bytes()
            with self.assertRaises(FileExistsError):
                subject.write_json(path, {"plan": "different"})
            self.assertEqual(path.read_bytes(), before)

    def test_missing_source_is_never_created(self):
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "absent.db"
            with self.assertRaises(FileNotFoundError):
                subject.read_only(path)
            self.assertFalse(path.exists())


if __name__ == "__main__":
    unittest.main()
