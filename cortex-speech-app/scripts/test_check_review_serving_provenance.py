#!/usr/bin/env python3
"""Regression tests for the live review-serving champion provenance gate."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT_DIR = Path(__file__).resolve().parent


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


gate = _load(
    "check_review_serving_provenance_under_test",
    SCRIPT_DIR / "check_review_serving_provenance.py",
)


class Fixture:
    def __init__(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "cortex-speech.db"
        self.conn = sqlite3.connect(self.path)
        self.conn.executescript(
            """
            CREATE TABLE model_versions (
                id TEXT NOT NULL,
                family TEXT NOT NULL,
                checkpoint_sha256 TEXT NOT NULL,
                status TEXT NOT NULL
            );
            CREATE TABLE speech_segments (
                id TEXT PRIMARY KEY,
                raw_transcript TEXT,
                human_decision TEXT,
                verified INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE segment_hypotheses (
                segment_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                transcript TEXT
            );
            CREATE TABLE review_events (segment_id TEXT NOT NULL);
            CREATE TABLE decision_log (segment_id TEXT NOT NULL, human_decision TEXT);
            """
        )

    def close(self):
        self.conn.close()
        self.temp.cleanup()

    def champion(self, model_id: str, sha256: str):
        self.conn.execute(
            "INSERT INTO model_versions VALUES (?, ?, ?, 'champion')",
            (model_id, gate.ASR_CHAMPION_FAMILY, sha256),
        )
        self.conn.commit()

    def segment(self, segment_id: str, raw: str, model_id: str, hypothesis: str):
        self.conn.execute(
            "INSERT INTO speech_segments(id, raw_transcript, human_decision, verified) VALUES (?, ?, '', 0)",
            (segment_id, raw),
        )
        self.conn.execute(
            "INSERT INTO segment_hypotheses VALUES (?, ?, ?)",
            (segment_id, model_id, hypothesis),
        )
        self.conn.commit()


class ChampionResolverTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.close()

    def test_exact_incumbent_admits_current_and_proven_alias(self):
        self.fx.champion(
            gate.PROVEN_LEGACY_CANONICAL_MODEL_ID,
            gate.PROVEN_LEGACY_DEPLOYMENT_SHA256,
        )
        self.assertEqual(
            gate.current_champion_model_ids(self.fx.conn),
            (gate.PROVEN_LEGACY_CANONICAL_MODEL_ID, gate.LEGACY_ALIAS_MODEL_ID),
        )

    def test_same_id_with_wrong_deployment_does_not_admit_alias(self):
        self.fx.champion(gate.PROVEN_LEGACY_CANONICAL_MODEL_ID, "1" * 64)
        self.assertEqual(
            gate.current_champion_model_ids(self.fx.conn),
            (gate.PROVEN_LEGACY_CANONICAL_MODEL_ID,),
        )

    def test_rotated_champion_does_not_admit_alias(self):
        self.fx.champion("omniasr-7b-new", "2" * 64)
        self.assertEqual(gate.current_champion_model_ids(self.fx.conn), ("omniasr-7b-new",))

    def test_zero_champions_fails_closed(self):
        with self.assertRaisesRegex(gate.ChampionRegistryError, "exactly one"):
            gate.current_champion_model_ids(self.fx.conn)

    def test_two_champions_fail_closed_even_if_schema_is_broken(self):
        self.fx.champion("champion-a", "3" * 64)
        self.fx.champion("champion-b", "4" * 64)
        with self.assertRaisesRegex(gate.ChampionRegistryError, "found 2"):
            gate.current_champion_model_ids(self.fx.conn)

    def test_malformed_id_or_sha_fails_closed(self):
        invalid = [
            ("bad id", "5" * 64, "invalid model id"),
            ("champion", "ABCDEF" + "0" * 58, "canonical deployment SHA-256"),
            ("champion", "short", "canonical deployment SHA-256"),
        ]
        for model_id, sha256, message in invalid:
            with self.subTest(model_id=model_id, sha256=sha256):
                self.fx.conn.execute("DELETE FROM model_versions")
                self.fx.conn.execute(
                    "INSERT INTO model_versions VALUES (?, ?, ?, 'champion')",
                    (model_id, gate.ASR_CHAMPION_FAMILY, sha256),
                )
                self.fx.conn.commit()
                with self.assertRaisesRegex(gate.ChampionRegistryError, message):
                    gate.current_champion_model_ids(self.fx.conn)

    def test_main_names_registry_failure_and_returns_nonzero(self):
        output = io.StringIO()
        with mock.patch.object(sys, "argv", ["check_review_serving_provenance.py", str(self.fx.path)]):
            with contextlib.redirect_stdout(output):
                result = gate.main()
        self.assertEqual(result, 1)
        self.assertIn("FAIL [champion-registry]", output.getvalue())
        self.assertIn("expected exactly one omniasr-7b champion, found 0", output.getvalue())


class ChampionFallbackTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()
        self.fx.champion(
            gate.PROVEN_LEGACY_CANONICAL_MODEL_ID,
            gate.PROVEN_LEGACY_DEPLOYMENT_SHA256,
        )

    def tearDown(self):
        self.fx.close()

    def failures(self):
        allowed = gate.current_champion_model_ids(self.fx.conn)
        return gate.non_champion_fallbacks(self.fx.conn, allowed)

    def test_current_champion_matching_transcript_passes(self):
        self.fx.segment("current", " دەقی ڕاست ", gate.PROVEN_LEGACY_CANONICAL_MODEL_ID, "دەقی ڕاست")
        self.assertEqual(self.failures(), [])

    def test_proven_legacy_alias_matching_transcript_passes(self):
        self.fx.segment("legacy", "دەقی کۆن", gate.LEGACY_ALIAS_MODEL_ID, " دەقی کۆن ")
        self.assertEqual(self.failures(), [])

    def test_unknown_model_fails_even_when_its_text_matches(self):
        self.fx.segment("unknown", "same words", "omniasr-ctc-300m", "same words")
        self.assertEqual(self.failures(), [("unknown", "no champion hypothesis")])

    def test_champion_transcript_mismatch_fails(self):
        self.fx.segment("mismatch", "served words", gate.PROVEN_LEGACY_CANONICAL_MODEL_ID, "different words")
        self.assertEqual(self.failures(), [("mismatch", "raw != champion")])

    def test_rotation_invalidates_legacy_alias_but_accepts_new_current(self):
        self.fx.conn.execute("UPDATE model_versions SET status = 'rolled_back'")
        self.fx.conn.execute(
            "INSERT INTO model_versions VALUES (?, ?, ?, 'champion')",
            ("omniasr-7b-new", gate.ASR_CHAMPION_FAMILY, "6" * 64),
        )
        self.fx.conn.commit()
        self.fx.segment("legacy-after-rotation", "old words", gate.LEGACY_ALIAS_MODEL_ID, "old words")
        self.fx.segment("new-current", "new words", "omniasr-7b-new", "new words")
        self.assertEqual(self.failures(), [("legacy-after-rotation", "no champion hypothesis")])


if __name__ == "__main__":
    unittest.main()
