#!/usr/bin/env python3
"""Unit tests for build_fleurs_ckb_manifest.write_manifest (P2.1).

Exercises the clip-writing/manifest core with synthetic FLEURS-shaped examples (numpy arrays), so no
1-2 GB HuggingFace download is needed. Requires numpy + soundfile (already deps of scorecard_finetuned).
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

try:
    import numpy as np  # noqa: E402

    from build_fleurs_ckb_manifest import write_manifest  # noqa: E402
except ImportError as exc:  # pragma: no cover - exercised only on bare runners
    # This is a build-tooling test for the FLEURS ckb manifest writer; it needs the optional
    # scientific stack (numpy + soundfile, the scorecard_finetuned deps). Hosted CI runners (the
    # Release Gate) deliberately don't install them -- the same design choice that keeps the large
    # gitignored ONNX models off hosted runners (see ci.yml's e2e:real note). Skip loudly rather
    # than hard-fail the whole policy gate; the test runs in FULL on any dev/scorecard machine that
    # has the deps, which is where the manifest writer is actually exercised.
    print(f"SKIP fleurs-manifest tests: optional dependency unavailable ({exc})", flush=True)
    raise SystemExit(0)


def _ex(clip_id, samples, sr, transcription=None, raw=None):
    ex = {"id": clip_id, "audio": {"array": np.zeros(samples, dtype=np.float32), "sampling_rate": sr}}
    if transcription is not None:
        ex["transcription"] = transcription
    if raw is not None:
        ex["raw_transcription"] = raw
    return ex


def test_write_manifest_writes_clips_and_manifest() -> None:
    with tempfile.TemporaryDirectory() as d:
        out = Path(d)
        examples = [
            _ex("c1", 16000, 16000, transcription="سڵاو"),
            _ex("c2", 0, 16000, transcription="نیە"),  # empty audio -> skipped
            _ex("c3", 8000, 16000, transcription="  "),  # empty reference -> skipped
        ]
        n, skipped = write_manifest(examples, out, wsl=False)
        assert n == 1, "only the clip with audio + non-empty ref is written"
        assert skipped == 2
        lines = (out / "fleurs_ckb_iq_frozen.tsv").read_text(encoding="utf-8").strip().splitlines()
        assert len(lines) == 1
        path, ref = lines[0].split("\t")
        assert path.endswith("c1.wav")
        assert ref == "سڵاو"
        assert (out / "clips" / "c1.wav").is_file(), "the clip WAV was written"
        sha = (out / "fleurs_ckb_iq_frozen.tsv.sha256").read_text(encoding="utf-8")
        assert len(sha.split()[0]) == 64


def test_write_manifest_falls_back_to_raw_transcription() -> None:
    with tempfile.TemporaryDirectory() as d:
        out = Path(d)
        n, _ = write_manifest([_ex("r1", 16000, 16000, raw="خاو")], out, wsl=False)
        assert n == 1
        ref = (out / "fleurs_ckb_iq_frozen.tsv").read_text(encoding="utf-8").split("\t")[1].strip()
        assert ref == "خاو", "raw_transcription is used when transcription is absent"


def test_write_manifest_disambiguates_duplicate_ids() -> None:
    # FLEURS shares one `id` across multiple recordings of a sentence; each must become its own clip
    # + row (no clobbering, no duplicate manifest rows). Regression for the 922-rows/348-unique defect.
    with tempfile.TemporaryDirectory() as d:
        out = Path(d)
        examples = [
            _ex("77", 16000, 16000, transcription="alpha"),
            _ex("77", 16000, 16000, transcription="alpha"),  # same id + ref (same FLEURS sentence)
            _ex("77", 16000, 16000, transcription="alpha"),
        ]
        n, skipped = write_manifest(examples, out, wsl=False)
        assert n == 3 and skipped == 0, "every same-id recording is written"
        lines = (out / "fleurs_ckb_iq_frozen.tsv").read_text(encoding="utf-8").strip().splitlines()
        paths = [ln.split("\t")[0] for ln in lines]
        assert len(paths) == len(set(paths)) == 3, f"same-id clips must get distinct paths: {paths}"
        for p in paths:
            assert (out / "clips" / Path(p).name).is_file(), f"clip WAV not written (clobbered?): {p}"


def test_write_manifest_limit_caps_rows() -> None:
    with tempfile.TemporaryDirectory() as d:
        out = Path(d)
        examples = [_ex("a", 16000, 16000, transcription="x"), _ex("b", 16000, 16000, transcription="y")]
        n, _ = write_manifest(examples, out, wsl=False, limit=1)
        assert n == 1, "the --limit cap is honored"


def _run() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            fn()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {name}: {exc}", flush=True)
        else:
            print(f"ok   {name}", flush=True)
    if failures:
        print(f"\n{failures} fleurs-manifest test(s) failed", flush=True)
        return 1
    print("\nfleurs-manifest tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
