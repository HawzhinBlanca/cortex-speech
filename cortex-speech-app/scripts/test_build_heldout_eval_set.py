#!/usr/bin/env python3
"""The held-out eval set is disjoint from training by text and by audio, or it is not written."""

from __future__ import annotations

import difflib
import json
import random
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402

from build_heldout_eval_set import (  # noqa: E402
    FUZZY_THRESHOLD,
    TrainingText,
    audio_overlaps,
    canon,
    leak_key,
    local_path,
    main as heldout_main,
)
from build_eval_slices import SliceBuildError  # noqa: E402
from promotion_gate import _slice_problems, inspect_eval_manifest, load_slices  # noqa: E402


def test_canon_folds_the_variants_the_audited_gate_folds() -> None:
    assert canon("كتێب، يەک!") == "کتێب یەک"
    assert leak_key("سڵاو  لە\u200cتۆ ٣") == leak_key("سڵاو لەتۆ 3")
    assert local_path("/mnt/d/a b/c.wav") == ("D:/a b/c.wav" if sys.platform == "win32" else "/mnt/d/a b/c.wav")


def test_text_leak_is_found_exactly_and_through_drift() -> None:
    training = TrainingText([leak_key("ئەمڕۆ کەشوهەوا زۆر خۆشە بۆ گەشت")])
    assert training.match(leak_key("ئەمڕۆ کەشوهەوا زۆر خۆشە بۆ گەشت")) is not None
    assert training.match(leak_key("ئەمڕۆ  كەشوهەوا زۆر خۆشە، بۆ گەشت!")) is not None  # kaf, spacing, punct
    assert training.match(leak_key("ئەمڕۆ کەشوهەوا زۆر خۆشە بۆ گەشتێک")) is not None  # one-word drift
    assert training.match(leak_key("سبەی دەچمە بازاڕ بۆ کڕینی نان")) is None
    assert training.match("") is None


def test_retrieval_never_misses_a_pair_the_exact_predicate_accepts() -> None:
    """The prefilter is an upper bound: brute force over random near-strings must agree exactly."""
    rng = random.Random(7)
    alphabet = "ابپتجچحخدرڕزژسشعغفڤقکگلڵمنوۆهەیێ"
    base = ["".join(rng.choice(alphabet) for _ in range(rng.randint(3, 40))) for _ in range(300)]
    training_texts = []
    for text in base:
        chars = list(text)
        for _ in range(rng.randint(0, 6)):
            chars[rng.randrange(len(chars))] = rng.choice(alphabet)
        training_texts.append("".join(chars))
    training = TrainingText(training_texts)
    for query in base:
        brute = any(
            difflib.SequenceMatcher(None, query, other).ratio() >= FUZZY_THRESHOLD for other in set(training_texts)
        )
        assert (training.match(query) is not None) == brute, query


def test_audio_overlap_is_byte_identity_and_missing_training_audio_refuses() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        ours, same, same_size, other = (root / name for name in ("ours.wav", "same.wav", "size.wav", "other.wav"))
        ours.write_bytes(b"RIFF-clip-bytes")
        same.write_bytes(b"RIFF-clip-bytes")
        same_size.write_bytes(b"RIFF-clip-bytez")
        other.write_bytes(b"different length")
        from policy_python import sha256_file

        mine = {str(ours): sha256_file(ours)}
        hits = audio_overlaps(mine, [str(same), str(same_size), str(other)])
        assert [Path(hit[1]).name for hit in hits] == ["same.wav"]
        assert audio_overlaps(mine, [str(same_size), str(other)]) == []
        try:
            audio_overlaps(mine, [str(root / "gone.wav")])
        except SliceBuildError as error:
            assert "cannot be proven" in str(error)
        else:
            raise AssertionError("a missing training audio file must refuse the build")


def _library(root: Path, sentences: list[str]) -> Path:
    """One 16 kHz source recording, one human-decided clip per second, plus decoys that never qualify."""
    source = root / "source-a.wav"
    sf.write(str(source), np.zeros(16000 * (len(sentences) + 1), dtype="float32"), 16000, subtype="PCM_16")
    db = root / "library.db"
    connection = sqlite3.connect(db)
    connection.execute(
        "CREATE TABLE speech_segments (id TEXT PRIMARY KEY, audio_path TEXT NOT NULL, snr_db REAL, "
        "speaker_id TEXT, audio_content_hash TEXT, human_decision TEXT, verdict_transcript TEXT, alignment_json TEXT)"
    )
    for i, text in enumerate(sentences):
        alignment = json.dumps({"source_start_ms": i * 1000, "source_end_ms": i * 1000 + 900})
        connection.execute(
            "INSERT INTO speech_segments VALUES (?,?,?,?,?,?,?,?)",
            (f"seg-{i:03d}", str(source), None, "SPEAKER_00", "content-a", "accept", text, alignment),
        )
    connection.execute(
        "INSERT INTO speech_segments VALUES ('undecided', ?, NULL, 'SPEAKER_00', 'content-a', NULL, NULL, ?)",
        (str(source), json.dumps({"source_start_ms": 0, "source_end_ms": 900})),
    )
    connection.execute(
        "INSERT INTO speech_segments VALUES ('rejected', ?, NULL, 'SPEAKER_00', 'content-a', 'reject', 'x', ?)",
        (str(source), json.dumps({"source_start_ms": 0, "source_end_ms": 900})),
    )
    connection.commit()
    connection.close()
    return db


def _sentence(i: int) -> str:
    """Seeded random Sorani-letter words: no two are near-duplicates of each other."""
    rng = random.Random(i)
    return " ".join("".join(rng.choice("ابپتجچحخدرڕزژسشعغفڤقکگلڵمنوۆهەیێ") for _ in range(6)) for _ in range(5))


def test_end_to_end_excludes_leaks_and_writes_a_gate_bound_set() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        sentences = [_sentence(i) for i in range(34)]
        db = _library(root, sentences)
        train_audio = root / "train.wav"
        sf.write(str(train_audio), np.ones(1600, dtype="float32") * 0.1, 16000, subtype="PCM_16")
        manifest = root / "train.jsonl"
        manifest.write_text(
            "".join(
                json.dumps({"audio_path": str(train_audio), "sentence": text}, ensure_ascii=False) + "\n"
                for text in (sentences[0], sentences[1] + "!", "هیچ پەیوەندییەکی نییە")
            ),
            encoding="utf-8",
        )
        out = root / "out"
        assert heldout_main(["--db", str(db), "--train-manifest", str(manifest), "--out-dir", str(out)]) == 0
        proof = json.loads((out / "heldout_eval.proof.json").read_text(encoding="utf-8"))
        assert proof["counts"]["text_leaked_or_empty"] == 2, proof["counts"]
        selected = {clip["segment_id"] for clip in proof["clips"]}
        assert len(selected) == 32 and not selected & {"seg-000", "seg-001", "undecided", "rejected"}
        assert proof["training_manifests"][0]["rows"] == 3

        eval_manifest = inspect_eval_manifest(out / "heldout_eval.tsv")
        assert proof["eval_manifest_sha256"] == eval_manifest.sha256 and eval_manifest.row_count == 32
        slices = load_slices(out / "eval_slices.tsv")
        assert _slice_problems(slices, eval_manifest.sha256, eval_manifest.row_count) == []


def test_audio_identical_to_a_training_file_is_excluded() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        db = _library(root, [_sentence(i) for i in range(31)])
        # A training row whose audio IS the library source recording: every clip of it is leaked.
        manifest = root / "train.jsonl"
        manifest.write_text(
            json.dumps({"audio_path": str(root / "source-a.wav"), "text": "بێ پەیوەندی"}, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        out = root / "out"
        assert heldout_main(["--db", str(db), "--train-manifest", str(manifest), "--out-dir", str(out)]) == 2
        assert not (out / "heldout_eval.tsv").exists()


def test_too_few_clips_refuses_and_writes_no_eval_set() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        db = _library(root, [_sentence(i) for i in range(29)])
        manifest = root / "train.jsonl"
        manifest.write_text(json.dumps({"audio_path": str(root / "source-a.wav"), "text": "x"}) + "\n", encoding="utf-8")
        out = root / "out"
        assert heldout_main(["--db", str(db), "--train-manifest", str(manifest), "--out-dir", str(out)]) == 2
        assert not (out / "heldout_eval.tsv").exists() and not (out / "heldout_eval.proof.json").exists()


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    failures = 0
    for test in tests:
        try:
            test()
        except Exception as error:  # noqa: BLE001 - report every arm
            failures += 1
            print(f"FAIL {test.__name__}: {error!r}", flush=True)
        else:
            print(f"  ok {test.__name__}", flush=True)
    if failures:
        print(f"HELD-OUT EVAL SET: {failures}/{len(tests)} test(s) FAILED", flush=True)
        return 1
    print(f"HELD-OUT EVAL SET: {len(tests)} disjointness arms pinned", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
