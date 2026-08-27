#!/usr/bin/env python3
"""Build mandatory, manifest-bound protected slices for model promotion.

Every eval row must resolve to exactly one live library segment.  Each resolved row is represented
in four honest dimensions:

* ``source:<hash>`` — a recording/content identity, never a collision-prone basename;
* ``speaker:<source-hash>:<label>`` — the source-scoped diarizer label, or ``unassigned``;
* ``dialect:<name>`` — the explicit dialect map, or the protected ``unmapped`` bucket;
* ``audio:clean|noisy|unmeasured`` — measured SNR, with missing measurement kept visible.

No discovered group is silently dropped.  If a group is smaller than ``--min-clips`` (never below
five), a row is unresolved, the manifest is too small, or no slices exist, generation is REFUSED and
an existing output is not overwritten.  The output records the exact eval-manifest and library-index
SHA-256 identities consumed by the build; ``promotion_gate.py`` verifies that binding and full row
coverage.

Usage::

    python scripts/build_eval_slices.py GOLD.tsv [--db cortex-speech.db] [--out eval_slices.tsv]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sqlite3
import sys
import tempfile
from collections import defaultdict
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Mapping, Sequence
from urllib.parse import quote

from promotion_gate import (
    EvalManifest,
    GateInputError,
    MIN_PAIRED_CLIPS,
    MIN_SLICE_CLIPS,
    SLICE_SCHEMA_VERSION,
    inspect_eval_manifest,
)

REPO_ROOT = Path(__file__).resolve().parents[1]
NOISY_SNR_DB = 15.0


class SliceBuildError(GateInputError):
    """The available evidence cannot support honest protected slices."""


@dataclass(frozen=True)
class LibraryClip:
    segment_id: str
    source_path: str
    snr_db: float | None
    speaker_id: str | None
    audio_content_hash: str | None


def _data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def _basename(path: str) -> str:
    return re.split(r"[\\/]", path)[-1]


def _stem(path: str) -> str:
    base = _basename(path)
    return base.rsplit(".", 1)[0] if "." in base else base


def source_dialects() -> list[tuple[str, str]]:
    """Parse dialect.rs's explicit SOURCE_DIALECTS source of truth."""
    try:
        dialect_rs = (REPO_ROOT / "src-tauri/src/dialect.rs").read_text(encoding="utf-8")
    except OSError as error:
        raise SliceBuildError(f"cannot read dialect source of truth: {error}") from error
    constants = dict(re.findall(r'pub const ([A-Z_]+): &str = "([a-z-]+)";', dialect_rs))
    block = re.search(r"const SOURCE_DIALECTS:[^=]*=\s*&\[(.*?)\n\];", dialect_rs, re.S)
    if not block:
        raise SliceBuildError("could not find SOURCE_DIALECTS in dialect.rs")
    pairs: list[tuple[str, str]] = []
    # Rust raw strings treat a trailing backslash literally.  Parsing them with the normal-string
    # escape rule drops exactly the folder mappings (all three end in ``\``), leaving only filename
    # fragments protected.  Capture raw and ordinary literals separately.
    literal_pattern = re.compile(
        r'\(\s*(?:r"([^"]*)"|"((?:[^"\\]|\\.)*)")\s*,\s*([A-Za-z_]+)\s*\)'
    )
    for raw_fragment, ordinary_fragment, name in literal_pattern.findall(block.group(1)):
        fragment = raw_fragment if raw_fragment else ordinary_fragment.replace("\\\\", "\\")
        dialect = constants.get(name)
        if dialect is None:
            raise SliceBuildError(f"SOURCE_DIALECTS references unknown dialect constant {name}")
        pairs.append((fragment, dialect))
    if not pairs:
        raise SliceBuildError("SOURCE_DIALECTS contains zero mappings")
    return pairs


def dialect_of(audio_path: str, table: Sequence[tuple[str, str]]) -> str | None:
    normalized = audio_path.replace("/", "\\")
    name = _basename(normalized)
    for fragment, dialect in table:
        if fragment and (fragment in name or fragment in normalized):
            return dialect
    return None


def _canonical_index_hash(clips: Sequence[LibraryClip]) -> str:
    records = [asdict(clip) for clip in sorted(clips, key=lambda item: item.segment_id)]
    payload = json.dumps(records, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def library_index(db_path: Path | None = None) -> tuple[dict[str, LibraryClip], str, list[str]]:
    """Return only unambiguous segment-id/path lookup keys plus an exact source-data hash."""
    db = db_path or (_data_dir() / "cortex-speech.db")
    if not db.is_file():
        raise SliceBuildError(f"library database does not exist: {db}")
    try:
        connection = sqlite3.connect(f"file:{db.resolve().as_posix()}?mode=ro", uri=True)
        try:
            raw_rows = connection.execute(
                "SELECT id, audio_path, snr_db, speaker_id, audio_content_hash "
                "FROM speech_segments ORDER BY id"
            ).fetchall()
        finally:
            connection.close()
    except sqlite3.Error as error:
        raise SliceBuildError(f"cannot read required library fields from {db}: {error}") from error
    if not raw_rows:
        raise SliceBuildError(f"library database {db} has zero speech segments")

    clips: list[LibraryClip] = []
    for row_number, (segment_id, source_path, snr, speaker, content_hash) in enumerate(raw_rows, 1):
        if (
            not isinstance(segment_id, str)
            or not segment_id.strip()
            or segment_id != segment_id.strip()
            or any(ord(character) < 32 or ord(character) == 127 for character in segment_id)
        ):
            raise SliceBuildError(f"library row {row_number} has an invalid segment id")
        if (
            not isinstance(source_path, str)
            or not source_path.strip()
            or source_path != source_path.strip()
            or any(ord(character) < 32 or ord(character) == 127 for character in source_path)
        ):
            raise SliceBuildError(f"library segment {segment_id!r} has no source audio path")
        if snr is not None:
            try:
                snr = float(snr)
            except (TypeError, ValueError) as error:
                raise SliceBuildError(f"library segment {segment_id!r} has invalid SNR {snr!r}") from error
            if not math.isfinite(snr):
                raise SliceBuildError(f"library segment {segment_id!r} has non-finite SNR")
        if speaker is not None:
            if (
                not isinstance(speaker, str)
                or not speaker.strip()
                or speaker != speaker.strip()
                or any(ord(character) < 32 or ord(character) == 127 for character in speaker)
            ):
                raise SliceBuildError(f"library segment {segment_id!r} has an invalid speaker label")
        if content_hash is not None:
            if (
                not isinstance(content_hash, str)
                or not content_hash.strip()
                or content_hash != content_hash.strip()
                or any(ord(character) < 32 or ord(character) == 127 for character in content_hash)
            ):
                raise SliceBuildError(f"library segment {segment_id!r} has an invalid audio content hash")
        clips.append(LibraryClip(segment_id, source_path, snr, speaker, content_hash))

    candidates: dict[str, set[LibraryClip]] = defaultdict(set)
    for clip in clips:
        for key in {clip.segment_id, _basename(clip.source_path), _stem(clip.source_path)}:
            if key:
                candidates[key].add(clip)
    index: dict[str, LibraryClip] = {}
    ambiguous: list[str] = []
    for key, matches in candidates.items():
        if len(matches) == 1:
            index[key] = next(iter(matches))
        else:
            ambiguous.append(key)
    return index, _canonical_index_hash(clips), sorted(ambiguous)


def _source_identity(clip: LibraryClip) -> str:
    # Content identity survives rename/relink/re-encode.  Full normalized path is the honest fallback
    # for pre-backfill rows; hashing keeps local paths and owner names out of the artifact.
    material = clip.audio_content_hash or clip.source_path.replace("/", "\\").casefold()
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def _safe_label(label: str) -> str:
    return quote(label, safe="-_.")


def build(
    manifest_paths: Sequence[str],
    index: Mapping[str, LibraryClip],
    dialects: Sequence[tuple[str, str]],
) -> tuple[dict[str, list[int]], list[int]]:
    """Build every honest group.  Unknown values become protected buckets, never omissions."""
    slices: dict[str, list[int]] = defaultdict(list)
    unresolved: list[int] = []
    for row_index, clip_path in enumerate(manifest_paths):
        matches = {
            match
            for key in (_stem(clip_path), _basename(clip_path), clip_path)
            if (match := index.get(key)) is not None
        }
        if len(matches) != 1:
            unresolved.append(row_index)
            continue
        clip = next(iter(matches))
        source = _source_identity(clip)
        dialect = dialect_of(clip.source_path, dialects) or "unmapped"
        speaker = _safe_label(clip.speaker_id or "unassigned")
        if clip.snr_db is None:
            audio_bucket = "unmeasured"
        else:
            audio_bucket = "noisy" if clip.snr_db < NOISY_SNR_DB else "clean"
        slices[f"source:{source}"].append(row_index)
        slices[f"speaker:{source}:{speaker}"].append(row_index)
        slices[f"dialect:{dialect}"].append(row_index)
        slices[f"audio:{audio_bucket}"].append(row_index)
    return dict(slices), unresolved


def validate_built_slices(
    slices: Mapping[str, Sequence[int]],
    unresolved: Sequence[int],
    manifest_rows: int,
    min_clips: int,
) -> list[str]:
    problems: list[str] = []
    if manifest_rows < MIN_PAIRED_CLIPS:
        problems.append(
            f"eval manifest has {manifest_rows} row(s); promotion requires at least {MIN_PAIRED_CLIPS}"
        )
    if min_clips < MIN_SLICE_CLIPS:
        problems.append(f"--min-clips cannot be below the mandatory floor {MIN_SLICE_CLIPS}")
    if unresolved and len(unresolved) == manifest_rows:
        # EVERY row missing is a different diagnosis from SOME rows missing, and reporting them the
        # same way costs an investigation. Measured 2026-08-19: the challenger cycle points this at
        # the frozen FLEURS manifest, whose clips live outside the library by design (canon: FLEURS
        # is never imported), so all 348 rows resolved to nothing. Read as a data-integrity fault it
        # sends you looking through speech_segments for corruption that is not there; the real
        # finding is that library-derived slices cannot be built from a manifest of non-library
        # audio at all.
        problems.append(
            f"none of the {manifest_rows} manifest row(s) name a clip in this library — protected "
            "slices are derived from library metadata (speaker, dialect, SNR, source), so an "
            "external eval set cannot be sliced this way"
        )
    elif unresolved:
        problems.append(
            f"{len(unresolved)} manifest row(s) did not resolve to exactly one library segment "
            f"(first: {list(unresolved)[:5]})"
        )
    if not slices:
        problems.append("zero protected slices were produced")
        return problems
    expected = set(range(manifest_rows))
    covered: set[int] = set()
    for name, clips in sorted(slices.items()):
        unique = set(clips)
        if len(unique) != len(clips):
            problems.append(f"slice {name!r} contains duplicate clip indices")
        if len(unique) < min_clips:
            problems.append(
                f"slice {name!r} has only {len(unique)} clip(s), below mandatory minimum {min_clips}"
            )
        outside = sorted(unique - expected)
        if outside:
            problems.append(f"slice {name!r} cites out-of-manifest clips {outside[:5]}")
        covered.update(unique & expected)
    missing = sorted(expected - covered)
    if missing:
        problems.append(f"{len(missing)} manifest row(s) have no protected slice (first: {missing[:5]})")
    return problems


def render_slices(
    manifest: EvalManifest,
    slices: Mapping[str, Sequence[int]],
    min_clips: int,
    index_sha256: str,
) -> str:
    lines = [
        f"# schema_version={SLICE_SCHEMA_VERSION}",
        f"# eval_manifest_sha256={manifest.sha256}",
        f"# eval_manifest_rows={manifest.row_count}",
        f"# min_clips={min_clips}",
        f"# library_index_sha256={index_sha256}",
        "slice_name\tclip_index",
    ]
    for name in sorted(slices):
        for clip in sorted(slices[name]):
            lines.append(f"{name}\t{clip}")
    return "\n".join(lines) + "\n"


def _write_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Build fail-closed protected promotion slices")
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--db", type=Path, help="read-only cortex-speech.db (defaults to the live library)")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--min-clips", type=int, default=MIN_SLICE_CLIPS)
    args = parser.parse_args(argv)
    out = args.out or args.manifest.with_name("eval_slices.tsv")

    try:
        manifest = inspect_eval_manifest(args.manifest)
        index, index_sha256, ambiguous = library_index(args.db)
        slices, unresolved = build(manifest.clip_paths, index, source_dialects())
        problems = validate_built_slices(slices, unresolved, manifest.row_count, args.min_clips)
        if problems:
            raise SliceBuildError("; ".join(problems))
        content = render_slices(manifest, slices, args.min_clips, index_sha256)
        _write_atomic(out, content)
    except (GateInputError, OSError) as error:
        print(f"EVAL SLICES: REFUSED - {error}", file=sys.stderr, flush=True)
        if out.exists():
            print(f"  existing output was not overwritten: {out}", file=sys.stderr, flush=True)
        return 2

    print(
        f"EVAL SLICES: OK - {len(slices)} mandatory group(s), {manifest.row_count} fully covered row(s)",
        flush=True,
    )
    for name in sorted(slices):
        print(f"  {name:80s} {len(slices[name]):5d} clips", flush=True)
    if ambiguous:
        print(
            f"  note: {len(ambiguous)} ambiguous basename/stem lookup key(s) excluded; all manifest "
            "rows still resolved uniquely by another key",
            flush=True,
        )
    print(f"  eval manifest sha256: {manifest.sha256}", flush=True)
    print(f"  library index sha256: {index_sha256}", flush=True)
    print(f"  written atomically to {out}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
