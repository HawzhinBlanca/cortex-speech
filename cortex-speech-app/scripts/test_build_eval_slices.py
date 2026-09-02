#!/usr/bin/env python3
"""Pure and file-level tests for protected eval-slice construction."""

from __future__ import annotations

import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_eval_slices import (  # noqa: E402
    LibraryClip,
    _source_identity,
    build,
    library_index,
    main as slices_main,
    render_slices,
    source_dialects,
    validate_built_slices,
)
from promotion_gate import EvalManifest, load_slices  # noqa: E402

MANIFEST_SHA = "a" * 64
INDEX_SHA = "b" * 64
N = 30


def clips_for_two_sources() -> dict[str, LibraryClip]:
    index: dict[str, LibraryClip] = {}
    for clip in range(N):
        first = clip < 15
        source = (
            r"D:\Kurdish Corpora\sorani\source-a.wav"
            if first
            else r"D:\corpus\KBHP-source-b.wav"
        )
        item = LibraryClip(
            segment_id=f"seg-{clip}",
            source_path=source,
            snr_db=25.0 if first else 8.0,
            speaker_id="SPEAKER_00" if first else "SPEAKER_01",
            audio_content_hash="content-a" if first else "content-b",
        )
        index[item.segment_id] = item
    return index


def manifest_paths(n: int = N) -> list[str]:
    return [f"clips/seg-{clip}.wav" for clip in range(n)]


def _create_db(path: Path, *, small_source: bool = False) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute(
            "CREATE TABLE speech_segments ("
            "id TEXT PRIMARY KEY, audio_path TEXT NOT NULL, snr_db REAL, "
            "speaker_id TEXT, audio_content_hash TEXT)"
        )
        for clip in range(N):
            split = 4 if small_source else 15
            first = clip < split
            source = (
                r"D:\Kurdish Corpora\sorani\source-a.wav"
                if first
                else r"D:\corpus\KBHP-source-b.wav"
            )
            connection.execute(
                "INSERT INTO speech_segments VALUES (?, ?, ?, ?, ?)",
                (
                    f"seg-{clip}",
                    source,
                    25.0 if first else 8.0,
                    "SPEAKER_00" if first else "SPEAKER_01",
                    "content-a" if first else "content-b",
                ),
            )
        connection.commit()
    finally:
        connection.close()


def _write_manifest(path: Path) -> None:
    path.write_text("".join(f"clips/seg-{clip}.wav\ttruth {clip}\n" for clip in range(N)), encoding="utf-8")


def test_build_emits_source_speaker_dialect_and_audio_slices() -> None:
    index = clips_for_two_sources()
    slices, unresolved = build(manifest_paths(), index, source_dialects())
    assert unresolved == []
    assert {"dialect:sorani", "dialect:hawleri", "audio:clean", "audio:noisy"} <= set(slices)
    assert sum(name.startswith("source:") for name in slices) == 2
    assert sum(name.startswith("speaker:") for name in slices) == 2
    assert validate_built_slices(slices, unresolved, N, 5) == []
    assert all(len(values) == 15 for values in slices.values())


def test_missing_metadata_is_explicitly_protected_not_dropped() -> None:
    source = r"D:\unknown\recording.wav"
    item = LibraryClip("", source, None, None, None)
    index = {}
    for clip in range(N):
        row = LibraryClip(f"seg-{clip}", source, None, None, None)
        index[row.segment_id] = row
    slices, unresolved = build(manifest_paths(), index, source_dialects())
    source_hash = _source_identity(item)
    assert unresolved == []
    assert slices["dialect:unmapped"] == list(range(N))
    assert slices["audio:unmeasured"] == list(range(N))
    assert slices[f"speaker:{source_hash}:unassigned"] == list(range(N))
    assert slices[f"source:{source_hash}"] == list(range(N))


def test_unresolved_rows_and_zero_slices_fail_closed() -> None:
    slices, unresolved = build(manifest_paths(), {}, source_dialects())
    problems = validate_built_slices(slices, unresolved, N, 5)
    assert any("name a clip in this library" in problem for problem in problems)
    assert any("zero protected slices" in problem for problem in problems)


def test_a_wholly_foreign_manifest_is_diagnosed_differently_from_a_few_bad_rows() -> None:
    """All-missing and some-missing are different faults, and one message for both costs a hunt.

    Measured 2026-08-19 against the live library: the challenger cycle's default eval manifest is
    the frozen FLEURS set, whose 348 clips are external by canon, so every row resolved to nothing.
    "348 row(s) did not resolve to exactly one library segment" reads as corruption in
    speech_segments and sends you looking for rows that are fine.
    """
    every_row_foreign = validate_built_slices({}, list(range(N)), N, 5)
    assert any("name a clip in this library" in problem for problem in every_row_foreign)
    assert not any("did not resolve" in problem for problem in every_row_foreign)

    index = clips_for_two_sources()
    slices, unresolved = build(manifest_paths()[:-1] + ["nowhere/absent.wav"], index, source_dialects())
    some_rows_bad = validate_built_slices(slices, unresolved, N, 5)
    assert any("did not resolve" in problem for problem in some_rows_bad), some_rows_bad
    assert not any("name a clip in this library" in problem for problem in some_rows_bad)


def test_discovered_group_under_five_is_not_silently_dropped() -> None:
    slices = {
        "source:small": list(range(4)),
        "source:large": list(range(4, N)),
        "audio:all": list(range(N)),
    }
    problems = validate_built_slices(slices, [], N, 5)
    assert any("source:small" in problem and "below mandatory minimum" in problem for problem in problems)


def test_manifest_under_thirty_and_requested_floor_under_five_are_refused() -> None:
    slices = {"all": list(range(29))}
    problems = validate_built_slices(slices, [], 29, 4)
    assert any("at least 30" in problem for problem in problems)
    assert any("cannot be below" in problem for problem in problems)


def test_rendered_slices_round_trip_with_exact_manifest_binding() -> None:
    slices, unresolved = build(manifest_paths(), clips_for_two_sources(), source_dialects())
    assert not unresolved
    manifest = EvalManifest(MANIFEST_SHA, N, tuple(manifest_paths()))
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "slices.tsv"
        path.write_text(render_slices(manifest, slices, 5, INDEX_SHA), encoding="utf-8")
        loaded = load_slices(path)
    assert loaded.metadata["eval_manifest_sha256"] == MANIFEST_SHA
    assert loaded.metadata["eval_manifest_rows"] == str(N)
    assert loaded.metadata["library_index_sha256"] == INDEX_SHA
    assert {name: list(values) for name, values in loaded.slices.items()} == {
        name: sorted(values) for name, values in slices.items()
    }


def test_library_index_hashes_exact_rows_and_excludes_ambiguous_basenames() -> None:
    with tempfile.TemporaryDirectory() as directory:
        db = Path(directory) / "library.db"
        _create_db(db)
        first, first_hash, ambiguous = library_index(db)
        second, second_hash, _ = library_index(db)
    assert first_hash == second_hash and len(first_hash) == 64
    assert first == second
    assert all(f"seg-{clip}" in first for clip in range(N))
    assert {"source-a.wav", "KBHP-source-b.wav"} <= set(ambiguous)
    assert "source-a.wav" not in first and "KBHP-source-b.wav" not in first


def test_cli_writes_a_gate_accepted_manifest_bound_file() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        db, manifest, out = root / "library.db", root / "gold.tsv", root / "slices.tsv"
        _create_db(db)
        _write_manifest(manifest)
        assert slices_main([str(manifest), "--db", str(db), "--out", str(out)]) == 0
        loaded = load_slices(out)
        assert loaded.slices
        assert loaded.metadata["eval_manifest_rows"] == str(N)
        assert all(len(set(values)) >= 5 for values in loaded.slices.values())


def test_cli_refusal_does_not_overwrite_existing_output() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        db, manifest, out = root / "library.db", root / "gold.tsv", root / "slices.tsv"
        _create_db(db, small_source=True)
        _write_manifest(manifest)
        sentinel = "previous-valid-artifact\n"
        out.write_text(sentinel, encoding="utf-8")
        assert slices_main([str(manifest), "--db", str(db), "--out", str(out)]) == 2
        assert out.read_text(encoding="utf-8") == sentinel


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    failures = 0
    for test in tests:
        try:
            test()
        except Exception as error:  # noqa: BLE001 - report every construction invariant
            failures += 1
            print(f"FAIL {test.__name__}: {error}", flush=True)
        else:
            print(f"  ok {test.__name__}", flush=True)
    if failures:
        print(f"EVAL SLICES: {failures}/{len(tests)} test(s) FAILED", flush=True)
        return 1
    print(f"EVAL SLICES: {len(tests)} fail-closed construction tests passed", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
