#!/usr/bin/env python3
"""Build a library held-out eval set PROVEN disjoint from the given training manifests.

The promotion gate needs >= 30 paired library clips whose protected slices (source, speaker,
dialect, audio) each hold >= 5 clips (``build_eval_slices.py``). The frozen FLEURS set cannot be
sliced (it is not library audio) and the old library-resident set was 11 clips. This tool selects
human-decided library clips, removes every clip that a training manifest already contains, prunes
groups that cannot reach the slice floor, cuts the survivors to WAVs and writes:

* ``heldout_eval.tsv``          ``<WSL wav path>\\t<reference>`` (what ``scorecard_7b.py`` reads)
* ``eval_slices.tsv``           the manifest-bound slices ``promotion_gate.py --slices`` verifies
* ``heldout_eval.proof.json``   manifest sha, every training manifest's sha + row count, the
                                method, per-stage counts and every clip's sha256

Eligible clip: a human verdict (``human_decision`` accept/edit with a non-empty
``verdict_transcript``, canon precedence "human verdict first"), stored source offsets, and a source
file libsndfile can read. The reference is that human verdict text.

Disjointness, both fail closed:

* TEXT — the audited omniasr-v11-audit fuzzy gate: canonical text with spaces removed, leaked when
  ``difflib.SequenceMatcher(None, a, b).ratio() >= 0.85`` against ANY training sentence. Candidate
  retrieval is NOT the char-4-gram heuristic (at 0.85 a shared-4-gram floor is not a necessary
  condition, so it can miss); it is the exact upper bound ``quick_ratio`` (length window + character
  histogram L1 <= 0.15 * total length), so no pair that reaches 0.85 is ever skipped.
* AUDIO — sha256 of every cut clip AND of its source recording against every training audio file.
  A different size proves a different sha256, so only same-size training files are hashed. A
  training audio file that cannot be found refuses the build: disjointness would be unproven.

Usage::

    python scripts/build_heldout_eval_set.py --train-manifest A.jsonl --train-manifest B.jsonl \\
        --out-dir runs/heldout_eval [--db cortex-speech.db] [--min-clips 5]

Exit 0 = written; 2 = refused (too few eligible clips, unreadable input, unproven disjointness).
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import re
import sqlite3
import sys
import unicodedata
from collections import defaultdict
from pathlib import Path
from typing import Iterable, Mapping, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_eval_slices import (  # noqa: E402
    SliceBuildError,
    _data_dir,
    build,
    library_index,
    render_slices,
    source_dialects,
    validate_built_slices,
)
from cortex_7b_client import win_to_wsl  # noqa: E402
from policy_python import sha256_file  # noqa: E402
from promotion_gate import MIN_PAIRED_CLIPS, MIN_SLICE_CLIPS, inspect_eval_manifest  # noqa: E402

PROOF_SCHEMA = 1
FUZZY_THRESHOLD = 0.85
_DIGITS = str.maketrans("٠١٢٣٤٥٦٧٨٩۰۱۲۳۴۵۶۷۸۹", "01234567890123456789")


def canon(text: str) -> str:
    """Byte-for-byte the leakage normalizer of omniasr-v11-audit/scripts/audit_common.py ``canon``.

    ponytail: copied, not imported — that repo is not a dependency of Cortex.
    """
    s = unicodedata.normalize("NFC", text or "")
    s = s.replace("ك", "ک").replace("ي", "ی").replace("ى", "ی")
    s = re.sub("[ً-ْٰـ‌-‏؜‪-‮⁦-⁩]", "", s).translate(_DIGITS)
    s = "".join(" " if unicodedata.category(c)[0] in "PS" else c for c in s)
    return " ".join(s.split())


def leak_key(text: str) -> str:
    return canon(text).replace(" ", "")


def local_path(path: str) -> str:
    """A WSL ``/mnt/<drive>/...`` manifest path, as this Windows process sees it."""
    match = re.match(r"^/mnt/([a-zA-Z])/(.*)$", path)
    if os.name == "nt" and match:
        return f"{match.group(1).upper()}:/{match.group(2)}"
    return path


class TrainingText:
    """Every training sentence, searchable for ``SequenceMatcher.ratio() >= 0.85`` with no misses."""

    def __init__(self, texts: Iterable[str]) -> None:
        import numpy as np

        self.np = np
        unique = sorted({t for t in texts if t}, key=lambda t: (len(t), t))
        self.exact = set(unique)
        self.texts = unique
        self.lengths = np.asarray([len(t) for t in unique], dtype=np.int64)
        alphabet = sorted({c for t in unique for c in t})
        self.index = {c: i for i, c in enumerate(alphabet)}
        self.hist = np.zeros((len(unique), len(alphabet)), dtype=np.int32)
        for row, text in enumerate(unique):
            for c in text:
                self.hist[row, self.index[c]] += 1

    def match(self, key: str) -> str | None:
        """The first training sentence ``key`` leaks into, or None. ``key`` is ``leak_key`` output."""
        if not key:
            return None
        if key in self.exact:
            return key
        np = self.np
        n = len(key)
        # ratio <= real_quick_ratio = 2*min/T, so 40*min >= 17*T is necessary (0.85 = 17/20).
        lo = int(np.searchsorted(self.lengths, -(-17 * n // 23)))
        hi = int(np.searchsorted(self.lengths, (23 * n) // 17, side="right"))
        if lo >= hi:
            return None
        query = np.zeros(self.hist.shape[1], dtype=np.int32)
        outside = 0
        for c in key:
            i = self.index.get(c)
            if i is None:
                outside += 1
            else:
                query[i] += 1
        # ratio <= quick_ratio = (T - L1)/T over character multisets, so 20*L1 <= 3*T is necessary.
        l1 = np.abs(self.hist[lo:hi] - query).sum(axis=1) + outside
        total = self.lengths[lo:hi] + n
        for offset in np.nonzero(20 * l1 <= 3 * total)[0]:
            other = self.texts[lo + int(offset)]
            if difflib.SequenceMatcher(None, key, other).ratio() >= FUZZY_THRESHOLD:
                return other
        return None


def load_training_manifests(paths: Sequence[Path]) -> tuple[list[str], list[str], list[dict]]:
    texts: list[str] = []
    audio: list[str] = []
    records: list[dict] = []
    for path in paths:
        rows = 0
        with path.open(encoding="utf-8") as handle:
            for line_number, line in enumerate(handle, 1):
                if not line.strip():
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError as error:
                    raise SliceBuildError(f"training manifest {path} line {line_number} is not JSON") from error
                text = row.get("text") or row.get("sentence")
                audio_path = row.get("audio_path")
                if not isinstance(text, str) or not isinstance(audio_path, str) or not audio_path:
                    raise SliceBuildError(f"training manifest {path} line {line_number} lacks text/audio_path")
                texts.append(leak_key(text))
                audio.append(local_path(audio_path))
                rows += 1
        records.append({"path": str(path), "sha256": sha256_file(path), "rows": rows})
    return texts, audio, records


def audio_overlaps(ours: Mapping[str, str], training_audio: Sequence[str]) -> list[tuple[str, str]]:
    """(our file, training file) pairs with identical sha256. ``ours`` maps path -> sha256.

    Sizes come from one directory listing per folder; only same-size training files are hashed.
    """
    sizes: dict[int, set[str]] = defaultdict(set)
    for path, digest in ours.items():
        sizes[os.path.getsize(path)].add(digest)
    by_dir: dict[str, set[str]] = defaultdict(set)
    for path in set(training_audio):
        by_dir[os.path.dirname(path)].add(os.path.basename(path))
    missing: list[str] = []
    hits: list[tuple[str, str]] = []
    digest_owner = {digest: path for path, digest in ours.items()}
    for folder, names in sorted(by_dir.items()):
        try:
            listing = {entry.name: entry for entry in os.scandir(folder or ".")}
        except OSError:
            missing.extend(os.path.join(folder, name) for name in names)
            continue
        for name in sorted(names):
            entry = listing.get(name)
            if entry is None or not entry.is_file():
                missing.append(os.path.join(folder, name))
                continue
            wanted = sizes.get(entry.stat().st_size)
            if wanted:
                digest = sha256_file(Path(entry.path))
                if digest in wanted:
                    hits.append((digest_owner[digest], entry.path))
    if missing:
        raise SliceBuildError(
            f"{len(missing)} training audio file(s) are missing (first: {missing[:3]}) - audio disjointness "
            "cannot be proven"
        )
    return hits


def library_candidates(db: Path) -> list[tuple[str, str, str, int, int]]:
    """(segment_id, source_path, reference, start_ms, end_ms) for every human-decided clip."""
    connection = sqlite3.connect(f"file:{db.resolve().as_posix()}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "SELECT id, audio_path, verdict_transcript, alignment_json FROM speech_segments "
            "WHERE human_decision IN ('accept','edit') AND trim(coalesce(verdict_transcript,'')) <> '' "
            "ORDER BY id"
        ).fetchall()
    finally:
        connection.close()
    out = []
    for segment_id, source, reference, alignment in rows:
        try:
            meta = json.loads(alignment or "{}")
            start, end = int(meta["source_start_ms"]), int(meta["source_end_ms"])
        except (ValueError, TypeError, KeyError, AttributeError):
            continue
        if 0 <= start < end:
            out.append((segment_id, source, " ".join(reference.split()), start, end))
    return out


def prune_to_slices(ids: Sequence[str], index, dialects, min_clips: int) -> tuple[list[str], dict[str, list[int]]]:
    """Drop every clip of an undersized slice until all slices reach ``min_clips`` (a fixpoint)."""
    kept = list(ids)
    while True:
        slices, unresolved = build(kept, index, dialects)
        if unresolved:
            raise SliceBuildError(f"{len(unresolved)} candidate(s) do not resolve to one library segment")
        small = {row for clips in slices.values() if len(clips) < min_clips for row in clips}
        if not small:
            return kept, slices
        kept = [segment for row, segment in enumerate(kept) if row not in small]


def cut_clip(source: str, start_ms: int, end_ms: int, out: Path) -> None:
    import soundfile as sf

    info = sf.info(source)
    start = start_ms * info.samplerate // 1000
    stop = min(end_ms * info.samplerate // 1000, info.frames)
    if stop <= start:
        raise SliceBuildError(f"{out.stem}: offsets fall outside {source}")
    data, rate = sf.read(source, start=start, stop=stop, always_2d=False)
    sf.write(str(out), data, rate, subtype="PCM_16")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Build a held-out library eval set disjoint from training")
    parser.add_argument("--train-manifest", action="append", type=Path, required=True, dest="train_manifests")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--db", type=Path, help="read-only cortex-speech.db (defaults to the live library)")
    parser.add_argument("--min-clips", type=int, default=MIN_SLICE_CLIPS)
    args = parser.parse_args(argv)
    db = args.db or (_data_dir() / "cortex-speech.db")
    counts: dict[str, int] = {}
    try:
        if args.min_clips < MIN_SLICE_CLIPS:
            raise SliceBuildError(f"--min-clips cannot be below the mandatory floor {MIN_SLICE_CLIPS}")
        index, index_sha256, _ambiguous = library_index(db)
        dialects = source_dialects()
        texts, training_audio, manifests = load_training_manifests(args.train_manifests)
        training = TrainingText(texts)
        candidates = library_candidates(db)
        counts["human_decided_with_offsets"] = len(candidates)

        import soundfile as sf

        readable, unreadable = [], 0
        for candidate in candidates:
            try:
                sf.info(candidate[1])
                readable.append(candidate)
            except Exception:  # noqa: BLE001 - any libsndfile refusal makes the clip uncuttable
                unreadable += 1
        counts["source_not_readable"] = unreadable
        clean = [c for c in readable if leak_key(c[2]) and training.match(leak_key(c[2])) is None]
        counts["text_leaked_or_empty"] = len(readable) - len(clean)
        by_id = {c[0]: c for c in clean}

        clips_dir = args.out_dir / "clips"
        clips_dir.mkdir(parents=True, exist_ok=True)
        audio_excluded: set[str] = set()
        while True:
            ids, _ = prune_to_slices(sorted(set(by_id) - audio_excluded), index, dialects, args.min_clips)
            counts["selected"] = len(ids)
            if len(ids) < MIN_PAIRED_CLIPS:
                raise SliceBuildError(
                    f"only {len(ids)} eligible clip(s) survive the slice floor; the gate needs "
                    f"{MIN_PAIRED_CLIPS} - counts: {counts}"
                )
            ours: dict[str, str] = {}
            for segment_id in ids:
                _, source, _, start, end = by_id[segment_id]
                clip = clips_dir / f"{segment_id}.wav"
                if not clip.is_file():
                    cut_clip(source, start, end, clip)
                ours[str(clip)] = sha256_file(clip)
            sources = {by_id[s][1]: sha256_file(Path(by_id[s][1])) for s in ids}
            leaked = audio_overlaps({**ours, **sources}, training_audio)
            if not leaked:
                break
            leaked_files = {ours_path for ours_path, _ in leaked}
            audio_excluded |= {s for s in ids if str(clips_dir / f"{s}.wav") in leaked_files or by_id[s][1] in leaked_files}
        counts["audio_leaked"] = len(audio_excluded)
        counts["pruned_by_slice_floor"] = len(by_id) - len(audio_excluded) - len(ids)
        kept = set(ids)
        for stale in clips_dir.glob("*.wav"):
            if stale.stem not in kept:
                stale.unlink()

        tsv = args.out_dir / "heldout_eval.tsv"
        rows = [f"{win_to_wsl(str((clips_dir / f'{s}.wav').resolve()))}\t{by_id[s][2]}" for s in ids]
        tsv.write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")
        manifest = inspect_eval_manifest(tsv)
        slices, unresolved = build(manifest.clip_paths, index, dialects)
        problems = validate_built_slices(slices, unresolved, manifest.row_count, args.min_clips)
        if problems:
            raise SliceBuildError("; ".join(problems))
        slices_path = args.out_dir / "eval_slices.tsv"
        slices_path.write_text(render_slices(manifest, slices, args.min_clips, index_sha256), encoding="utf-8", newline="\n")
        proof = {
            "schema": PROOF_SCHEMA,
            "eval_manifest_sha256": manifest.sha256,
            "eval_manifest_rows": manifest.row_count,
            "eval_slices_sha256": sha256_file(slices_path),
            "library_index_sha256": index_sha256,
            "training_manifests": manifests,
            "method": {
                "text": f"audit_common.canon, spaces removed, difflib ratio >= {FUZZY_THRESHOLD} vs every "
                "training sentence (quick_ratio bound retrieval, no misses)",
                "audio": "sha256 of every clip and its source recording vs every training audio file",
            },
            "counts": counts,
            "slices": {name: len(rows) for name, rows in sorted(slices.items())},
            "clips": [
                {"segment_id": s, "clip_sha256": ours[str(clips_dir / f"{s}.wav")], "source_sha256": sources[by_id[s][1]]}
                for s in ids
            ],
        }
        proof_path = args.out_dir / "heldout_eval.proof.json"
        proof_path.write_text(json.dumps(proof, indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    except (SliceBuildError, OSError, sqlite3.Error) as error:
        print(f"HELD-OUT EVAL SET: REFUSED - {error}", file=sys.stderr, flush=True)
        return 2

    print(f"HELD-OUT EVAL SET: OK - {manifest.row_count} clips, {len(slices)} protected slices", flush=True)
    print(f"  counts: {json.dumps(counts)}", flush=True)
    for name, size in proof["slices"].items():
        print(f"  {name:80s} {size:5d}", flush=True)
    print(f"  eval manifest sha256: {manifest.sha256}", flush=True)
    print(f"  proof: {proof_path} (sha256 {sha256_file(proof_path)})", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
