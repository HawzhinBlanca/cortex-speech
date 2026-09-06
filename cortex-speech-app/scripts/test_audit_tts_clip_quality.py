"""TTS audit safety regressions, discovered by the mandatory python-policies gate."""
import argparse
from array import array
import csv
from contextlib import redirect_stdout
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import wave

import audit_tts_clip_quality as audit


class TtsAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.folder = self.root / "wavs"
        self.folder.mkdir()
        self.args = argparse.Namespace(min_snr=5, max_clipping=0, min_similarity=.65, max_tail_silence=1)

    def wav(self, name="sample.wav", samples=None, channels=1):
        path = self.folder / name
        pcm = array("h", samples if samples is not None else [1, -1, 100, -100] * 1200)
        if sys.byteorder != "little":
            pcm.byteswap()
        with wave.open(str(path), "wb") as target:
            target.setparams((channels, 2, 24000, 0, "NONE", "not compressed"))
            target.writeframes(pcm.tobytes())
        return path

    def meta(self, rows):
        path = self.root / "metadata.csv"
        with path.open("w", encoding="utf-8-sig", newline="") as target:
            writer = csv.DictWriter(target, fieldnames=["audio_path", "similarity_score", "source_file"], delimiter="|")
            writer.writeheader()
            writer.writerows(rows)

    def cli(self, *extra):
        with patch.object(sys, "argv", ["audit", str(self.folder), "--out", str(self.root / "audit.csv"), *extra]):
            with redirect_stdout(io.StringIO()):
                return audit.main()

    def report(self):
        with (self.root / "audit.csv").open(encoding="utf-8", newline="") as source:
            return list(csv.DictReader(source))

    def test_synthetic_silence_is_hold_never_keep(self):
        original = self.wav(samples=[0] * 4800)
        before = original.read_bytes()
        self.assertEqual(self.cli(), 0)  # completed measurement, not a quality certificate
        row = self.report()[0]
        self.assertEqual(row["verdict"], "hold")
        self.assertIn("constant_or_silent_audio", row["reasons"])
        self.assertEqual(row["tts_gold_certified"], "False")
        self.assertEqual(before, original.read_bytes())

    def test_no_warnings_is_still_not_gold(self):
        row = dict(constant_signal=False, snr_db=20, clipping_ratio=0, tail_silence_s=0,
                   dc_offset=0, similarity=.9)
        self.assertEqual(audit.classify(row, self.args), ("pending_qualification", []))

    def test_missing_measurements_remain_unknown(self):
        row = dict(constant_signal=False, snr_db=None, clipping_ratio=0, tail_silence_s=0,
                   dc_offset=0, similarity=None)
        verdict, reasons = audit.classify(row, self.args)
        self.assertEqual(verdict, "hold")
        self.assertIn("snr_unmeasurable", reasons)
        self.assertIn("speaker_similarity_missing_or_invalid", reasons)

    def test_one_negative_rail_hit_not_rounded_to_zero(self):
        samples = [1] * 2_100_000
        samples[100] = -32768
        row = audit.measure(self.wav(samples=samples))
        self.assertEqual(row["clipped_samples"], 1)
        self.assertGreater(row["clipping_ratio"], 0)
        row["similarity"] = .9
        self.assertIn("clipping_candidate", audit.classify(row, self.args)[1])

    def test_all_unreadable_inputs_are_reported_without_crash(self):
        (self.folder / "broken.wav").write_bytes(b"not a WAV")
        self.assertEqual(self.cli(), 2)
        self.assertEqual(len(self.report()), 1)
        self.assertEqual(self.report()[0]["verdict"], "hold")
        self.assertIn("broken.wav", (self.root / "audit_flagged.txt").read_text())

    def test_mixed_readable_and_broken_preserves_all_rows(self):
        self.wav()
        (self.folder / "broken.wav").write_bytes(b"not a WAV")
        self.assertEqual(self.cli(), 2)
        self.assertEqual(len(self.report()), 2)

    def test_truncated_pcm_is_not_silently_accepted(self):
        path = self.wav()
        path.write_bytes(path.read_bytes()[:-2])
        self.assertIsNone(audit.measure(path))

    def test_stereo_is_not_downmixed_into_false_clean_signal(self):
        self.assertIsNone(audit.measure(self.wav(channels=2)))

    def test_duplicate_metadata_is_not_last_row_wins(self):
        self.wav()
        self.meta([dict(audio_path="wavs/sample.wav", similarity_score=score, source_file=source)
                   for score, source in [(".99", "episode-a"), (".8", "episode-b")]])
        with self.assertRaisesRegex(ValueError, "duplicate_metadata"):
            audit.load_similarity(self.folder)
        self.assertEqual(self.cli(), 2)
        self.assertIn("duplicate_metadata", self.report()[0]["reasons"])
        self.assertEqual(self.report()[0]["similarity"], "")

    def test_casefolded_duplicate_also_refused(self):
        self.wav()
        self.meta([dict(audio_path=f"wavs/{name}", similarity_score=".9")
                   for name in ["sample.wav", "SAMPLE.wav"]])
        with self.assertRaisesRegex(ValueError, "duplicate_metadata"):
            audit.load_similarity(self.folder)

    def test_valid_metadata_and_nonfinite_scores(self):
        self.wav()
        for score in ["nan", "inf", "-inf", "1.01", "-1.01", ""]:
            self.meta([dict(audio_path="wavs/sample.wav", similarity_score=score)])
            self.assertEqual(audit.load_similarity(self.folder), {})
        self.meta([dict(audio_path="wavs/sample.wav", similarity_score=".8")])
        self.assertEqual(audit.load_similarity(self.folder), {"sample.wav": .8})

    def test_metadata_missing_file_is_integrity_error(self):
        self.meta([dict(audio_path="wavs/missing.wav", similarity_score=".8")])
        with self.assertRaisesRegex(ValueError, "metadata_audio_missing"):
            audit.load_similarity(self.folder)

    def test_metadata_cannot_bind_another_directory_by_basename(self):
        self.wav()
        self.meta([dict(audio_path="elsewhere/sample.wav", similarity_score=".8")])
        with self.assertRaisesRegex(ValueError, "outside_requested_folder"):
            audit.load_similarity(self.folder)

    def test_output_evidence_not_overwritten(self):
        self.wav()
        self.assertEqual(self.cli(), 0)
        before = (self.root / "audit.csv").read_bytes()
        self.assertEqual(self.cli(), 2)
        self.assertEqual((self.root / "audit.csv").read_bytes(), before)

    def test_invalid_threshold_cannot_disable_comparisons(self):
        self.wav()
        with self.assertRaises(SystemExit):
            self.cli("--min-snr", "nan")


if __name__ == "__main__":
    unittest.main()
