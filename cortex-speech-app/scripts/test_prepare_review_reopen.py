#!/usr/bin/env python3
"""Deterministic synthetic tests; no production database or reviewer identity required."""

import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

from prepare_review_reopen import collect, prepare, verify_inventory


def fixture(path):
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    conn.executescript("""
        CREATE TABLE review_pool_registry(pool_id TEXT, focus_segment_count INTEGER);
        INSERT INTO review_pool_registry VALUES('pool',3);
        CREATE TABLE review_pool_members(pool_id TEXT, segment_id TEXT, voice_name TEXT,
          raw_transcript TEXT, audio_content_hash TEXT, source_start_ms INTEGER,
          source_end_ms INTEGER, duration_ms INTEGER);
        CREATE TABLE speech_segments(id TEXT, review_revision INTEGER, reviewed_by TEXT,
          human_decision TEXT, verified INTEGER, verdict_transcript TEXT, annotated_transcript TEXT);
        CREATE TABLE review_pool_duplicate_exclusions(pool_id TEXT, segment_id TEXT, canonical_segment_id TEXT);
        CREATE TABLE review_events(id INTEGER, segment_id TEXT, reviewer TEXT, action TEXT,
          requested_action TEXT, timestamp_ms INTEGER, operation_id TEXT, served_revision INTEGER);
        CREATE TABLE review_pool_decisions(id INTEGER, pool_id TEXT, segment_id TEXT, reviewer TEXT,
          action TEXT, requested_action TEXT, created_at_ms INTEGER, operation_id TEXT);
        CREATE TABLE review_pool_reversals(decision_id INTEGER);
        CREATE VIEW effective_review_pool_decisions_v62 AS SELECT * FROM review_pool_decisions d
          WHERE NOT EXISTS(SELECT 1 FROM review_pool_reversals r WHERE r.decision_id=d.id);
        CREATE TABLE effective_independent_review_decisions_v61(id INTEGER, segment_id TEXT);
        CREATE TABLE review_pool_owner_adjudications(id INTEGER, segment_id TEXT);
    """)
    for n in range(3):
        conn.execute("INSERT INTO review_pool_members VALUES('pool',?,'Voice','draft',?,0,1000,1000)",
                     (str(n), str(n) * 64))
        conn.execute("INSERT INTO speech_segments VALUES(?,1,'Sara','accept',1,'retained correction',NULL)", (str(n),))
    conn.commit()
    return conn


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "library #1.db"
        self.conn = fixture(self.path)

    def tearDown(self):
        self.conn.close()
        self.tmp.cleanup()

    def event(self, ident, segment, action, requested, name="Sara"):
        self.conn.execute("INSERT INTO review_events VALUES(?,?,?,?,?,10,?,0)",
                          (ident, segment, name, action, requested, f"op-{ident}"))

    def report(self):
        return collect(self.conn, [" Sara ", "sara"])

    def test_button_accept_after_edit_is_not_lost(self):
        self.event(1, "0", "edit", "accept")
        result = self.report()
        self.assertEqual(result["confirmed_looks_good_segment_ids"], ["0"])
        self.assertEqual(result["summary"]["sara"]["retained_corrected_then_looks_good"], 1)

    def test_save_next_unchanged_is_not_invented_as_looks_good(self):
        self.event(1, "0", "accept", "edit")
        result = self.report()
        row = next(r for r in result["rows"] if r["segment_id"] == "0")
        self.assertEqual(row["batch"], "other_history")
        self.assertNotIn("0", result["unproven_accept_segment_ids"])
        self.assertEqual(result["confirmed_looks_good_segment_ids"], [])

    def test_missing_button_provenance_is_explicit(self):
        self.event(1, "0", "accept", None)
        result = self.report()
        self.assertEqual(result["unproven_accept_segment_ids"], ["0", "1", "2"])
        self.assertEqual(result["confirmed_looks_good_segment_ids"], [])

    def test_skip_history_does_not_hide_unproven_current_accept(self):
        self.event(1, "0", "skip", "skip")
        self.assertIn("0", self.report()["unproven_accept_segment_ids"])

    def test_duplicate_and_outside_ids_are_not_reintroduced(self):
        self.event(1, "0", "accept", "accept")
        self.event(2, "outside", "accept", "accept")
        self.conn.execute("INSERT INTO review_pool_duplicate_exclusions VALUES('pool','0','1')")
        result = self.report()
        self.assertEqual(result["confirmed_looks_good_segment_ids"], [])
        self.assertEqual(result["summary"]["sara"]["excluded_duplicate_clips"], 1)
        self.assertEqual(result["summary"]["sara"]["outside_pool_clips"], 1)
        self.assertEqual(next(r for r in result["rows"] if r["segment_id"] == "0")["duplicate_target"], "1")

    def test_history_stays_when_pool_decision_is_reversed(self):
        self.conn.execute("INSERT INTO review_pool_decisions VALUES(7,'pool','0','Sara','accept','accept',10,'p7')")
        self.conn.execute("INSERT INTO review_pool_reversals VALUES(7)")
        result = self.report()
        self.assertEqual(result["confirmed_looks_good_segment_ids"], ["0"])
        self.assertEqual(result["summary"]["sara"]["retained_effective_pool_accepts"], 0)

    def test_later_correction_remains_bound_to_inventory(self):
        self.event(1, "0", "accept", "accept")
        self.event(2, "0", "edit", "edit")
        first = self.report()
        self.conn.execute("UPDATE speech_segments SET review_revision=2, annotated_transcript='new' WHERE id='0'")
        second = self.report()
        self.assertNotEqual(first["inventory_sha256"], second["inventory_sha256"])
        self.assertEqual(second["confirmed_looks_good_segment_ids"], ["0"])
        self.assertNotIn("retained correction", json.dumps(second))

    def test_named_reviewers_only_and_shared_clips_count_once(self):
        self.event(1, "0", "accept", "accept", "Sara")
        self.event(2, "0", "accept", "accept", "Hemn")
        self.event(3, "1", "accept", "accept", "Other")
        result = collect(self.conn, ["Sara", "Hemn"])
        self.assertEqual(result["confirmed_looks_good_segment_ids"], ["0"])
        self.assertEqual(set(result["summary"]), {"sara", "hemn"})

    def test_deterministic_and_never_authorizes_activation(self):
        first = self.report()
        self.assertEqual(first["inventory_sha256"], self.report()["inventory_sha256"])
        self.assertFalse(first["activation_allowed"])
        self.assertTrue(first["read_only"])

    def test_reads_special_character_path_without_mutation(self):
        self.event(1, "0", "accept", "accept")
        self.conn.commit()
        before = self.path.read_bytes()
        self.assertEqual(prepare(self.path, ["Sara"])["confirmed_looks_good_segment_ids"], ["0"])
        self.assertEqual(before, self.path.read_bytes())

    def test_missing_or_ambiguous_pool_fails_closed(self):
        self.conn.execute("DELETE FROM review_pool_registry")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.report()
        with self.assertRaises(ValueError):
            prepare(Path(self.tmp.name) / "missing.db", ["Sara"])
        self.assertFalse((Path(self.tmp.name) / "missing.db").exists())

    def test_registry_drift_and_empty_reviewer_fail(self):
        self.conn.execute("UPDATE review_pool_registry SET focus_segment_count=99")
        with self.assertRaisesRegex(ValueError, "mismatch"):
            self.report()
        with self.assertRaises(ValueError):
            collect(self.conn, [" "])

    def test_cli_refuses_existing_output(self):
        output = Path(self.tmp.name) / "existing.json"
        output.write_text("preserve", encoding="utf-8")
        run = subprocess.run([sys.executable, str(Path(__file__).with_name("prepare_review_reopen.py")),
                              "--db", str(self.path), "--reviewer", "Sara", "--output", str(output)],
                             capture_output=True, timeout=10)
        self.assertNotEqual(run.returncode, 0)
        self.assertEqual(output.read_text(encoding="utf-8"), "preserve")

    def test_freshness_rejects_tampered_and_changed_inventories(self):
        before = self.report()
        verify_inventory(before, self.report())
        tampered = json.loads(json.dumps(before))
        tampered["confirmed_looks_good_segment_ids"] = ["invented"]
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            verify_inventory(tampered, before)
        self.conn.execute("UPDATE speech_segments SET review_revision=2 WHERE id='0'")
        with self.assertRaisesRegex(ValueError, "stale"):
            verify_inventory(before, self.report())

    def test_other_reviewer_agreement_invalidates_saved_inventory(self):
        self.event(1, "0", "accept", "accept")
        before = self.report()
        self.conn.execute("INSERT INTO review_pool_decisions VALUES(8,'pool','0','Other','accept','accept',20,'other8')")
        with self.assertRaisesRegex(ValueError, "stale"):
            verify_inventory(before, self.report())

    def test_incomplete_or_activatable_inventory_refused(self):
        before = self.report()
        with self.assertRaisesRegex(ValueError, "preparation-only"):
            verify_inventory({**before, "activation_allowed": True}, before)
        incomplete = dict(before)
        del incomplete["authority_sha256"]
        with self.assertRaisesRegex(ValueError, "incomplete"):
            verify_inventory(incomplete, before)


if __name__ == "__main__":
    unittest.main()
