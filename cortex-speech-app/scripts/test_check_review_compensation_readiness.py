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
sys.path.insert(0, str(SCRIPT.parent))

from review_pilot_hidden_contract import HIDDEN_SCHEMA_SQL, policy_sha256, read_policy  # noqa: E402


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
        self.pilot_policy = {
            "schema_version": 1,
            "after_review_event_id": 0,
            "max_total_corpus_actions": 20,
            "reviewers": [
                {"name": "Rubar", "max_corpus_actions": 10},
                {"name": "Alle", "max_corpus_actions": 10},
            ],
        }
        (self.root / "review_pilot_policy.json").write_text(
            json.dumps(self.pilot_policy), encoding="utf-8"
        )
        (self.root / "couch_session.json").write_text(
            json.dumps(
                {
                    "db_path": str(self.db),
                    "reviewers": {"token-h": "Rubar", "token-p": "Alle"},
                    "pilot_policy": self.pilot_policy,
                    "pilot_spot_checks": [],
                }
            ),
            encoding="utf-8",
        )
        connection = sqlite3.connect(self.db)
        connection.executescript(
            """
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, description TEXT);
            INSERT INTO schema_migrations VALUES (60, 'reversible compensation pilot fixture');
            CREATE TABLE speech_segments (
                id TEXT PRIMARY KEY, audio_content_hash TEXT, alignment_json TEXT, duration_ms INTEGER
            );
            CREATE TABLE review_events (
                id INTEGER PRIMARY KEY, segment_id TEXT, reviewer TEXT, action TEXT,
                compensation_action TEXT, source TEXT, duration_ms INTEGER,
                operation_id TEXT, operation_payload_hash TEXT,
                timestamp_ms INTEGER DEFAULT 1700000000000,
                created_at TEXT DEFAULT '2026-08-22 07:00:00',
                app_git_sha TEXT DEFAULT 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                playback_guard_version TEXT DEFAULT 'content-hash-raw-counter-v3'
            );
            CREATE TABLE spot_checks (segment_id TEXT, reviewer TEXT, action TEXT);
            CREATE TABLE review_compensation_policies (
                policy_version TEXT PRIMARY KEY, effective_after_event_id INTEGER,
                base_rate_micro_iqd_per_hour INTEGER, edit_basis_points INTEGER,
                accept_basis_points INTEGER, reject_basis_points INTEGER, skip_basis_points INTEGER
            );
            CREATE UNIQUE INDEX idx_review_events_operation_id
              ON review_events(operation_id) WHERE operation_id IS NOT NULL;
            CREATE TABLE review_compensation_ledger (
                id INTEGER PRIMARY KEY, entry_id TEXT UNIQUE, entry_key TEXT, policy_version TEXT,
                review_event_id INTEGER, canonical_work_id TEXT, canonical_identity_kind TEXT,
                reviewer TEXT, segment_id TEXT, source TEXT,
                compensation_action TEXT, effective_decision TEXT, decision_revision INTEGER,
                duration_ms INTEGER, rate_basis_points INTEGER, entitlement_micro_iqd INTEGER,
                delta_micro_iqd INTEGER, corrected_entitlement_ms INTEGER,
                delta_corrected_ms INTEGER, created_at TEXT DEFAULT '2026-08-22 07:00:00',
                reverses_entry_id TEXT,
                FOREIGN KEY(policy_version) REFERENCES review_compensation_policies(policy_version),
                FOREIGN KEY(review_event_id) REFERENCES review_events(id)
            );
            CREATE UNIQUE INDEX idx_review_compensation_one_entry_per_event
              ON review_compensation_ledger(review_event_id) WHERE review_event_id IS NOT NULL;
            CREATE UNIQUE INDEX idx_review_compensation_one_reversal_per_entry
              ON review_compensation_ledger(reverses_entry_id) WHERE reverses_entry_id IS NOT NULL;
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
            CREATE TABLE review_effect_state (
                singleton_key INTEGER PRIMARY KEY,
                effective_after_review_event_id INTEGER,
                effective_after_ledger_id INTEGER,
                created_at TEXT DEFAULT '2026-08-22 07:00:00'
            );
            INSERT INTO review_effect_state VALUES (1, 0, 0, '2026-08-22 07:00:00');
            CREATE TABLE human_decision_effect_events (
                id INTEGER PRIMARY KEY,
                review_event_id INTEGER UNIQUE,
                segment_id TEXT,
                reviewer TEXT,
                source TEXT,
                action TEXT,
                decision_revision INTEGER,
                created_at TEXT DEFAULT '2026-08-22 07:00:00'
            );
            CREATE TABLE human_decision_effect_reversals (
                effect_event_id INTEGER PRIMARY KEY,
                operation_id TEXT UNIQUE,
                created_at TEXT DEFAULT '2026-08-22 07:00:00'
            );
            CREATE TRIGGER review_events_v60_provenance_validate_insert BEFORE INSERT ON review_events
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'invalid'); END;
            CREATE TRIGGER review_events_v60_provenance_immutable_update BEFORE UPDATE ON review_events
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_events_v60_post_cutoff_immutable_update BEFORE UPDATE ON review_events
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_events_v60_post_cutoff_immutable_delete BEFORE DELETE ON review_events
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_effect_state_immutable_insert BEFORE INSERT ON review_effect_state
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_effect_state_immutable_update BEFORE UPDATE ON review_effect_state
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER review_effect_state_immutable_delete BEFORE DELETE ON review_effect_state
              WHEN 0 BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER human_decision_effect_events_validate_review_event_insert
              BEFORE INSERT ON human_decision_effect_events WHEN 0
              BEGIN SELECT RAISE(ABORT, 'invalid'); END;
            CREATE TRIGGER human_decision_effect_events_immutable_update
              BEFORE UPDATE ON human_decision_effect_events WHEN 0
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER human_decision_effect_events_immutable_delete
              BEFORE DELETE ON human_decision_effect_events WHEN 0
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER human_decision_effect_reversals_validate_phone_insert
              BEFORE INSERT ON human_decision_effect_reversals WHEN 0
              BEGIN SELECT RAISE(ABORT, 'invalid'); END;
            CREATE TRIGGER human_decision_effect_reversals_immutable_update
              BEFORE UPDATE ON human_decision_effect_reversals WHEN 0
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            CREATE TRIGGER human_decision_effect_reversals_immutable_delete
              BEFORE DELETE ON human_decision_effect_reversals WHEN 0
              BEGIN SELECT RAISE(ABORT, 'immutable'); END;
            INSERT INTO speech_segments VALUES
              ('s1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               '{"source_start_ms":0,"source_end_ms":1000}', 1000);
            INSERT INTO review_compensation_policies VALUES
              ('review-iqd-v1-2026-08-21', 0, 18000000000, 10000, 1000, 1000, 0);
            INSERT INTO review_events
              (id, segment_id, reviewer, action, compensation_action, source, duration_ms,
               operation_id, operation_payload_hash)
            VALUES (1, 's1', 'Rubar', 'edit', 'edit', 'couch', 1000,
                    '11111111-1111-4111-8111-111111111111',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
            INSERT INTO review_compensation_ledger
              (id, entry_id, entry_key, policy_version, review_event_id, canonical_work_id,
               canonical_identity_kind, reviewer, segment_id, source, compensation_action,
               effective_decision, decision_revision, duration_ms, rate_basis_points,
               entitlement_micro_iqd, delta_micro_iqd, corrected_entitlement_ms,
               delta_corrected_ms, reverses_entry_id)
            VALUES (1, 'entry-1', 'review-event:1', 'review-iqd-v1-2026-08-21', 1,
                    'reviewer-work-v1:5:rubar:audio-segment-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:0:1000',
                    'audio_content_hash+source_span', 'Rubar', 's1', 'couch', 'edit', 'edit', 1,
                    1000, 10000, 5000000, 5000000, 1000, 1000, NULL);
            INSERT INTO human_decision_effect_events
              (id, review_event_id, segment_id, reviewer, source, action, decision_revision)
            VALUES (1, 1, 's1', 'Rubar', 'couch', 'edit', 1);
            CREATE VIEW effective_review_events_v60 AS
            SELECT e.id AS review_event_id, e.segment_id, e.reviewer, e.action, e.source,
                   e.timestamp_ms, e.created_at AS review_event_created_at,
                   e.duration_ms AS review_event_duration_ms,
                   e.compensation_action AS review_event_compensation_action,
                   e.operation_id, e.operation_payload_hash, e.app_git_sha,
                   e.playback_guard_version, l.id AS ledger_id, l.entry_id AS ledger_entry_id,
                   l.entry_key AS ledger_entry_key, l.policy_version, l.canonical_work_id,
                   l.canonical_identity_kind, l.reviewer AS ledger_reviewer,
                   l.segment_id AS ledger_segment_id, l.source AS ledger_source,
                   l.compensation_action AS ledger_compensation_action,
                   l.effective_decision, l.decision_revision,
                   l.duration_ms AS ledger_duration_ms, l.rate_basis_points,
                   l.entitlement_micro_iqd, l.delta_micro_iqd,
                   l.corrected_entitlement_ms, l.delta_corrected_ms,
                   l.created_at AS ledger_created_at
              FROM review_events e JOIN review_compensation_ledger l ON l.review_event_id=e.id
             WHERE l.reverses_entry_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM review_compensation_ledger r
                                WHERE r.reverses_entry_id=l.entry_id)
               AND NOT EXISTS (SELECT 1 FROM review_compensation_ledger newer
                                JOIN review_events ne ON ne.id=newer.review_event_id
                               WHERE newer.reverses_entry_id IS NULL
                                 AND newer.policy_version=l.policy_version
                                 AND newer.canonical_work_id=l.canonical_work_id
                                 AND ne.id>e.id
                                 AND NOT EXISTS (SELECT 1 FROM review_compensation_ledger rr
                                                  WHERE rr.reverses_entry_id=newer.entry_id));
            """
        )
        connection.executescript(HIDDEN_SCHEMA_SQL)
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

    def activate_flexible_pool(self, *, created_at: str = "2026-08-24 08:00:00") -> None:
        connection = sqlite3.connect(self.db)
        connection.executescript(
            """
            INSERT INTO schema_migrations VALUES (63, 'flexible pool fixture');
            INSERT INTO schema_migrations VALUES (64, 'dedup fixture');
            INSERT INTO schema_migrations VALUES (65, 'rights-lineage fixture');
            CREATE TABLE review_pool_registry(
                singleton_key INTEGER, pool_id TEXT, focus_segment_count INTEGER,
                focus_sha256 TEXT, created_at TEXT
            );
            CREATE TABLE review_pool_members(pool_id TEXT, segment_id TEXT);
            CREATE TABLE review_pool_decisions(id INTEGER, operation_id TEXT);
            CREATE TABLE review_pool_reversals(id INTEGER);
            INSERT INTO review_pool_members VALUES
                ('123e4567-e89b-42d3-a456-426614174000', 's1');
            """
        )
        connection.execute(
            "INSERT INTO review_pool_registry VALUES (1, ?, 1, ?, ?)",
            (
                "123e4567-e89b-42d3-a456-426614174000",
                "a" * 64,
                created_at,
            ),
        )
        connection.commit()
        connection.close()
        (self.root / "review_pilot_policy.json").unlink()

    def rewrite_all_durations(self, duration_ms: int) -> None:
        """Keep every pay artifact internally consistent while changing the server denominator."""
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute("UPDATE speech_segments SET duration_ms = ?", (duration_ms,))
        connection.execute("UPDATE review_events SET duration_ms = ?", (duration_ms,))
        connection.execute(
            """UPDATE review_compensation_ledger
                  SET duration_ms = ?, entitlement_micro_iqd = ?, delta_micro_iqd = ?,
                      corrected_entitlement_ms = ?, delta_corrected_ms = ?""",
            (duration_ms, duration_ms * 5_000, duration_ms * 5_000, duration_ms, duration_ms),
        )
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_update "
            "BEFORE UPDATE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()

    def insert_paid_event(
        self,
        connection: sqlite3.Connection,
        *,
        event_id: int,
        segment_id: str,
        reviewer: str = "Rubar",
        action: str = "accept",
        source: str = "couch",
    ) -> None:
        operation_id = f"{event_id:08x}-0000-4000-8000-{event_id:012x}"
        connection.execute(
            """INSERT INTO review_events
                   (id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                    operation_id, operation_payload_hash)
                 VALUES (?, ?, ?, ?, ?, ?, 1000, ?, ?)""",
            (
                event_id,
                segment_id,
                reviewer,
                action,
                action,
                source,
                operation_id,
                f"{event_id:064x}",
            ),
        )
        basis_points = 0 if action == "skip" else 1000
        entitlement = 0 if action == "skip" else 500_000
        connection.execute(
            """INSERT INTO review_compensation_ledger
                   (id, entry_id, entry_key, policy_version, review_event_id,
                    canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                    compensation_action, effective_decision, decision_revision, duration_ms,
                    rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 VALUES (?, ?, ?, 'review-iqd-v1-2026-08-21', ?, ?,
                         'audio_content_hash+source_span', ?, ?, ?, ?, ?, ?, 1000,
                         ?, ?, ?, 0, 0, NULL)""",
            (
                event_id,
                f"entry-{event_id}",
                f"review-event:{event_id}",
                event_id,
                f"reviewer-work-v1:{len(reviewer.lower())}:{reviewer.lower()}:audio-{segment_id}",
                reviewer,
                segment_id,
                source,
                action,
                action,
                event_id,
                basis_points,
                entitlement,
                entitlement,
            ),
        )
        if source == "couch" and action != "skip":
            connection.execute(
                """INSERT INTO human_decision_effect_events
                       (id, review_event_id, segment_id, reviewer, source, action, decision_revision)
                     VALUES (?, ?, ?, ?, ?, ?, ?)""",
                (event_id, event_id, segment_id, reviewer, source, action, event_id),
            )

    def append_exact_reversal(
        self,
        connection: sqlite3.Connection,
        *,
        event_id: int,
        operation_id: str,
    ) -> None:
        connection.execute(
            """INSERT INTO review_compensation_ledger
                   (id, entry_id, entry_key, policy_version, review_event_id,
                    canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                    compensation_action, effective_decision, decision_revision, duration_ms,
                    rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT 1000 + id, 'undo-entry-' || ?, 'undo:' || ?, policy_version, NULL,
                        canonical_work_id, canonical_identity_kind, reviewer, segment_id,
                        'couch_undo', 'undo', 'undo', decision_revision, duration_ms,
                        0, 0, -delta_micro_iqd, 0, -delta_corrected_ms, entry_id
                   FROM review_compensation_ledger
                  WHERE review_event_id = ? AND reverses_entry_id IS NULL""",
            (event_id, operation_id, event_id),
        )

    def append_initial_undo(
        self,
        connection: sqlite3.Connection,
        *,
        include_effect_reversal: bool,
    ) -> str:
        operation_id = "22222222-2222-4222-8222-222222222222"
        connection.execute(
            """INSERT INTO review_compensation_ledger
                   (id, entry_id, entry_key, policy_version, review_event_id,
                    canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                    compensation_action, effective_decision, decision_revision, duration_ms,
                    rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT 2, 'undo-entry-1', ?, policy_version, NULL,
                        canonical_work_id, canonical_identity_kind, reviewer, segment_id,
                        'couch_undo', 'undo', 'undo', decision_revision, duration_ms,
                        0, 0, -delta_micro_iqd, 0, -delta_corrected_ms, entry_id
                   FROM review_compensation_ledger WHERE entry_id='entry-1'""",
            (f"undo:{operation_id}",),
        )
        if include_effect_reversal:
            connection.execute(
                "INSERT INTO human_decision_effect_reversals VALUES (1, ?, '2026-08-22 07:01:00')",
                (operation_id,),
            )
        return operation_id

    def append_redo(self, connection: sqlite3.Connection) -> None:
        connection.execute(
            """INSERT INTO review_events
                   (id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                    operation_id, operation_payload_hash)
                 VALUES (2, 's1', 'Rubar', 'accept', 'accept', 'couch', 1000,
                         '33333333-3333-4333-8333-333333333333', ?)""",
            ("c" * 64,),
        )
        connection.execute(
            """INSERT INTO review_compensation_ledger
                   (id, entry_id, entry_key, policy_version, review_event_id,
                    canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                    compensation_action, effective_decision, decision_revision, duration_ms,
                    rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT 3, 'entry-2', 'review-event:2', policy_version, 2,
                        canonical_work_id, canonical_identity_kind, reviewer, segment_id,
                        'couch', 'accept', 'accept', 2, duration_ms,
                        1000, 500000, 500000, 0, 0, NULL
                   FROM review_compensation_ledger WHERE entry_id='entry-1'"""
        )
        connection.execute(
            """INSERT INTO human_decision_effect_events
                   (id, review_event_id, segment_id, reviewer, source, action, decision_revision)
                 VALUES (2, 2, 's1', 'Rubar', 'couch', 'accept', 2)"""
        )

    def test_valid_ledger_and_focus_pass(self):
        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertEqual(report["totalEarnedMicroIqd"], 5_000_000)
        self.assertEqual(report["correctedAudioMs"], 1000)
        self.assertEqual(report["schemaVersion"], 60)
        self.assertEqual(report["pilotCorpusActions"], 1)
        self.assertEqual(report["pilotHiddenActions"], 0)
        self.assertEqual(report["pilotUiActions"], 1)
        self.assertEqual(report["pilotHiddenGrants"], 0)

    def test_one_millisecond_endpoint_rounding_remains_payable(self):
        self.rewrite_all_durations(999)

        code, report = self.run_gate()

        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertEqual(report["totalEarnedMicroIqd"], 4_995_000)
        self.assertEqual(report["correctedAudioMs"], 999)

    def test_internally_consistent_tenfold_duration_cannot_mint_tenfold_pay(self):
        self.rewrite_all_durations(10_000)

        code, report = self.run_gate()

        self.assertEqual(code, 1, report)
        self.assertTrue(
            any("differs from exact source span length" in error for error in report["errors"]),
            report,
        )
        self.assertTrue(any("ledger canonical identity" in error for error in report["errors"]), report)

    def test_post_cutoff_event_outside_exact_active_focus_fails(self):
        self.focus.write_text(json.dumps({"segment_ids": ["different-segment"]}), encoding="utf-8")

        code, report = self.run_gate()

        self.assertEqual(code, 1, report)
        self.assertTrue(any("outside the exact active review focus" in error for error in report["errors"]), report)

    def test_undo_then_redo_counts_one_effective_action_and_preserves_exact_raw_history(self):
        connection = sqlite3.connect(self.db)
        self.append_initial_undo(connection, include_effect_reversal=True)
        self.append_redo(connection)
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertEqual(report["postCutoffEvents"], 1)
        self.assertEqual(report["rawPostCutoffEvents"], 2)
        self.assertEqual(report["ledgerEntries"], 1)
        self.assertEqual(report["rawLedgerEntries"], 3)
        self.assertEqual(report["reversalEntries"], 1)
        self.assertEqual(report["pilotCorpusActions"], 1)
        self.assertEqual(report["totalEarnedMicroIqd"], 500_000)
        self.assertEqual(report["correctedAudioMs"], 0)

    def test_later_decision_cannot_erase_an_earlier_skip_safety_slot(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DELETE FROM human_decision_effect_events WHERE review_event_id=1")
        connection.execute(
            "UPDATE review_events SET action='skip', compensation_action='skip' WHERE id=1"
        )
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute(
            """UPDATE review_compensation_ledger
                  SET compensation_action='skip', effective_decision='skip',
                      rate_basis_points=0, entitlement_micro_iqd=0, delta_micro_iqd=0,
                      corrected_entitlement_ms=0, delta_corrected_ms=0
                WHERE review_event_id=1"""
        )
        connection.execute(
            """CREATE TRIGGER review_compensation_ledger_immutable_update
                 BEFORE UPDATE ON review_compensation_ledger
                 BEGIN SELECT RAISE(ABORT, 'immutable'); END"""
        )
        self.append_redo(connection)
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertEqual(report["postCutoffEvents"], 2)
        self.assertEqual(report["rawPostCutoffEvents"], 2)
        self.assertEqual(report["pilotCorpusActions"], 2)
        self.assertEqual(report["reversalEntries"], 0)

    def test_phone_undo_without_exact_effect_reversal_is_rejected(self):
        connection = sqlite3.connect(self.db)
        self.append_initial_undo(connection, include_effect_reversal=False)
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(
            any("lacks its exact operation-bound effect reversal" in error for error in report["errors"]),
            report,
        )

    def test_phone_undo_with_forged_effect_operation_linkage_is_rejected(self):
        connection = sqlite3.connect(self.db)
        self.append_initial_undo(connection, include_effect_reversal=True)
        connection.execute(
            """UPDATE human_decision_effect_reversals
                  SET operation_id='44444444-4444-4444-8444-444444444444'
                WHERE effect_event_id=1"""
        )
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(
            any("lacks its exact operation-bound effect reversal" in error for error in report["errors"]),
            report,
        )

    def test_session_hidden_key_must_be_a_subset_of_durable_grants(self):
        session_path = self.root / "couch_session.json"
        session = json.loads(session_path.read_text(encoding="utf-8"))
        session["pilot_spot_checks"] = [["unreserved", "Rubar"]]
        session_path.write_text(json.dumps(session), encoding="utf-8")
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("unreserved key" in error for error in report["errors"]), report)

    def test_session_reviewer_roster_must_exactly_match_the_policy(self):
        session_path = self.root / "couch_session.json"
        session = json.loads(session_path.read_text(encoding="utf-8"))
        session["reviewers"] = {"token-r": "Rubar", "token-s": "Sewa"}
        session_path.write_text(json.dumps(session), encoding="utf-8")
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("unauthorized reviewer" in error for error in report["errors"]), report)

    def test_post_baseline_hidden_result_requires_the_exact_durable_grant(self):
        connection = sqlite3.connect(self.db)
        self.insert_paid_event(
            connection,
            event_id=2,
            segment_id="s1",
            action="accept",
            source="couch_spot_check",
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("no active durable reservation" in error for error in report["errors"]), report)
        self.assertFalse(any("effect rows" in error for error in report["errors"]), report)

    def test_hidden_spot_check_cannot_be_forged_into_a_human_decision_effect(self):
        connection = sqlite3.connect(self.db)
        self.insert_paid_event(
            connection,
            event_id=2,
            segment_id="hidden-effect-forbidden",
            action="edit",
            source="couch_spot_check",
        )
        connection.execute(
            """INSERT INTO human_decision_effect_events
                   (id, review_event_id, segment_id, reviewer, source, action, decision_revision)
                 VALUES (2, 2, 'hidden-effect-forbidden', 'Rubar',
                         'couch_spot_check', 'edit', 2)"""
        )
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(
            any("hidden spot-check event 2 unexpectedly has a human-decision effect" in error for error in report["errors"]),
            report,
        )

    def test_hidden_spot_check_history_is_immutable_and_cannot_be_reversed(self):
        connection = sqlite3.connect(self.db)
        self.insert_paid_event(
            connection,
            event_id=2,
            segment_id="hidden-reversal-forbidden",
            action="accept",
            source="couch_spot_check",
        )
        self.append_exact_reversal(
            connection,
            event_id=2,
            operation_id="44444444-4444-4444-8444-444444444444",
        )
        connection.commit()
        connection.close()

        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(
            any("hidden spot-check event 2 is immutable and cannot be reversed" in error for error in report["errors"]),
            report,
        )

    def test_hidden_event_and_spot_result_must_exist_together_and_agree(self):
        digest = policy_sha256(read_policy(self.root / "review_pilot_policy.json"))
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 0, 'Rubar', 'hidden-a')",
            (digest,),
        )
        self.insert_paid_event(
            connection,
            event_id=2,
            segment_id="hidden-a",
            action="accept",
            source="couch_spot_check",
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("event/result mismatch" in error for error in report["errors"]), report)

    def test_per_reviewer_cap_fails_before_total_ui_actions_can_exceed_24(self):
        connection = sqlite3.connect(self.db)
        for event_id in range(2, 12):
            self.insert_paid_event(
                connection,
                event_id=event_id,
                segment_id=f"work-{event_id}",
                action="skip",
            )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("10-action cap" in error for error in report["errors"]), report)

    def test_active_policy_sha_and_baseline_must_match_every_active_namespace_row(self):
        digest = policy_sha256(read_policy(self.root / "review_pilot_policy.json"))
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 1, 'Rubar', 'hidden-a')",
            (digest,),
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("SHA/baseline" in error for error in report["errors"]), report)

    def test_active_baseline_cannot_use_a_noncanonical_policy_sha(self):
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 0, 'Rubar', 'hidden-a')",
            ("f" * 64,),
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("SHA/baseline" in error for error in report["errors"]), report)

    def test_reserved_hidden_key_cannot_be_finalized_through_the_corpus_path(self):
        digest = policy_sha256(read_policy(self.root / "review_pilot_policy.json"))
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO review_pilot_hidden_keys VALUES (?, 0, 'Rubar', 's1')",
            (digest,),
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("finalized through the corpus path" in error for error in report["errors"]), report)

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
        self.assertTrue(
            any("event 1 has 0 original ledger rows" in error for error in report["errors"]),
            report,
        )

    def test_cross_policy_duplicate_event_ledger_is_visible_and_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP INDEX idx_review_compensation_one_entry_per_event")
        connection.execute(
            """INSERT INTO review_compensation_policies VALUES
               ('other-policy', 0, 18000000000, 10000, 1000, 1000, 0)"""
        )
        connection.execute(
            """INSERT INTO review_compensation_ledger
                   (id, entry_id, entry_key, policy_version, review_event_id,
                    canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                    compensation_action, effective_decision, decision_revision, duration_ms,
                    rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 VALUES (2, 'entry-2', 'review-event:1', 'other-policy', 1,
                    'reviewer-work-v1:5:rubar:audio-segment-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:0:1000',
                    'audio_content_hash+source_span', 'Rubar', 's1', 'couch', 'edit', 'edit', 1,
                    1000, 10000, 5000000, 0, 1000, 0, NULL)"""
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("2 original ledger rows" in error for error in report["errors"]), report)

    def test_event_ledger_unique_index_must_have_the_exact_partial_unique_shape(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP INDEX idx_review_compensation_one_entry_per_event")
        connection.execute(
            """CREATE INDEX idx_review_compensation_one_entry_per_event
                 ON review_compensation_ledger(review_event_id)"""
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertIn(
            "missing/malformed unique partial idx_review_compensation_one_entry_per_event",
            report["errors"],
        )

    def test_event_ledger_index_with_a_vacuous_partial_predicate_fails(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP INDEX idx_review_compensation_one_entry_per_event")
        connection.execute(
            """CREATE UNIQUE INDEX idx_review_compensation_one_entry_per_event
                 ON review_compensation_ledger(review_event_id)
                 WHERE review_event_id IS NOT NULL AND id < 0"""
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertIn(
            "missing/malformed unique partial idx_review_compensation_one_entry_per_event",
            report["errors"],
        )

    def test_ledger_work_id_must_derive_from_the_exact_event_segment_and_reviewer(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute("UPDATE review_compensation_ledger SET canonical_work_id = 'made-up-work'")
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_update BEFORE UPDATE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("ledger canonical identity" in error for error in report["errors"]), report)

    def test_ledger_identity_kind_must_match_the_derived_canonical_audio_identity(self):
        connection = sqlite3.connect(self.db)
        connection.execute("DROP TRIGGER review_compensation_ledger_immutable_update")
        connection.execute(
            "UPDATE review_compensation_ledger SET canonical_identity_kind = 'made-up-kind'"
        )
        connection.execute(
            "CREATE TRIGGER review_compensation_ledger_immutable_update BEFORE UPDATE ON review_compensation_ledger "
            "BEGIN SELECT RAISE(ABORT, 'immutable'); END"
        )
        connection.commit()
        connection.close()
        code, report = self.run_gate()
        self.assertEqual(code, 1, report)
        self.assertTrue(any("ledger canonical identity" in error for error in report["errors"]), report)

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
            (
                "s2",
                "a" * 64,
                '{"source_start_ms":0,"source_end_ms":1000}',
                1000,
            ),
        )
        connection.commit()
        connection.close()
        self.focus.write_text(json.dumps({"segment_ids": ["s1", "s2"]}), encoding="utf-8")
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("duplicate canonical pay identities" in error for error in report["errors"]), report)

    def test_flexible_pool_proves_deferred_compensation_without_legacy_pilot(self):
        self.activate_flexible_pool()
        code, report = self.run_gate()
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"], report)
        self.assertEqual(report["mode"], "flexible-pool")
        self.assertEqual(report["compensationOperationalStatus"], "deferred")
        self.assertEqual(report["postPoolLegacyReviewEvents"], 0)
        self.assertEqual(report["postPoolLegacyLedgerEntries"], 0)

    def test_flexible_pool_refuses_legacy_pay_events_after_activation(self):
        self.activate_flexible_pool(created_at="2026-08-21 00:00:00")
        code, report = self.run_gate()
        self.assertEqual(code, 1)
        self.assertTrue(any("legacy paid-review event" in error for error in report["errors"]), report)
        self.assertTrue(any("legacy compensation" in error for error in report["errors"]), report)

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
