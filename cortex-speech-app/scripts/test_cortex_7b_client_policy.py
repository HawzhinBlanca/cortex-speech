"""Unit test for cortex_7b_client.resolve_clip_offsets — the whole-file-vs-clip guard on the DEFAULT 7B path.

The WSL-7B transcribe path (the champion/default engine) invokes cortex_7b_client.py, which reads a segment's
alignment_json from the app DB and hands source offsets to the server for clipping. A present-but-offset-less
alignment (a clobbered chunk: a bare {"words": ...} array, unparseable JSON, or only one offset) must be
REFUSED — NOT sent with null offsets. Sending null offsets makes the server transcribe the ENTIRE source file
and store it as THIS one clip's transcript: whole-file-vs-clip training-data corruption. This mirrors the Rust
readers slice_for_export and slice_pcm_by_alignment, which already refuse the same shape. A genuine whole-file
segment carries NO alignment (import always writes source offsets, even for a chunk_count==1 single-file
segment), so refusing present-but-offset-less never rejects legitimate data.

Fail-before: reverting resolve_clip_offsets to the old `m.get(...)`-then-whole-file logic makes the
offset-less / partial / unparseable cases return (None, None), and the asserts below fire.
"""
import json
import sqlite3
import sys
import tempfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cortex_7b_client as client  # noqa: E402
from cortex_7b_client import (  # noqa: E402
    ClobberedAlignment,
    SegmentSnapshotError,
    read_segment_from_snapshot,
    resolve_clip_offsets,
)


def test_resolve_clip_offsets() -> None:
    # No alignment at all -> a genuine whole-file segment; whole-file is correct.
    assert resolve_clip_offsets(None) == (None, None)
    assert resolve_clip_offsets("") == (None, None)

    # Valid chunk offsets -> clip to that window.
    assert resolve_clip_offsets(json.dumps({"source_start_ms": 1000, "source_end_ms": 2500})) == (1000, 2500)
    # Offsets can coexist with a merged words array (the normal post-alignment shape).
    both = json.dumps({"source_start_ms": 0, "source_end_ms": 500, "words": [{"word": "x"}]})
    assert resolve_clip_offsets(both) == (0, 500)

    # Present-but-offset-less (clobbered chunk) -> REFUSE, never whole-file.
    for bad in (
        json.dumps([{"word": "x", "start": 0.0, "end": 1.0}]),  # bare word array
        json.dumps({"words": []}),                               # object without offsets
        json.dumps({"source_start_ms": 100}),                    # only start, no end
        json.dumps({"source_end_ms": 500}),                      # only end, no start
        "{not valid json",                                       # unparseable
    ):
        try:
            resolve_clip_offsets(bad)
        except ClobberedAlignment:
            pass
        else:
            raise AssertionError(f"resolve_clip_offsets must REFUSE a present-but-offset-less alignment: {bad!r}")


class _SnapshotCursor:
    def __init__(self, row):
        self._row = row

    def fetchone(self):
        return self._row


class _SnapshotConnection:
    def __init__(self, row):
        self._row = row

    def execute(self, _query, _parameters):
        return _SnapshotCursor(self._row)

    def close(self):
        pass


def test_wal_copy_failure_never_falls_back_to_main_file() -> None:
    """An observed-but-uncopyable WAL is an EX_DB-class snapshot failure after retries."""
    copies = []

    def fail_wal_copy(source, _destination):
        copies.append(source)
        if source.endswith("-wal"):
            raise OSError("simulated WAL copy failure")

    with (
        mock.patch.object(client.os.path, "exists", return_value=True),
        mock.patch.object(client.shutil, "copyfile", side_effect=fail_wal_copy),
        mock.patch.object(client.time, "sleep", return_value=None),
    ):
        try:
            read_segment_from_snapshot("source.db", "seg", attempts=3)
        except SegmentSnapshotError as exc:
            assert "simulated WAL copy failure" in str(exc)
        else:
            raise AssertionError("WAL copy failure must not become a main-file-only EX_NOSEG result")

    assert copies.count("source.db-wal") == 3, "the complete DB+WAL snapshot must be retried"


def test_real_wal_snapshot_exposes_committed_segment() -> None:
    """Exercise SQLite itself: the requested row exists only in the copied WAL, not the main file."""
    with tempfile.TemporaryDirectory(prefix="cortex7b_wal_policy_") as directory:
        db = str(Path(directory) / "source.db")
        writer = sqlite3.connect(db)
        try:
            assert writer.execute("PRAGMA journal_mode=WAL").fetchone()[0].lower() == "wal"
            writer.execute(
                "CREATE TABLE speech_segments (id TEXT PRIMARY KEY, audio_path TEXT, alignment_json TEXT)"
            )
            writer.commit()
            writer.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            writer.execute(
                "INSERT INTO speech_segments VALUES (?, ?, ?)",
                ("fresh", "audio.wav", '{"source_start_ms":0,"source_end_ms":10}'),
            )
            writer.commit()
            assert Path(db + "-wal").stat().st_size > 0

            row = read_segment_from_snapshot(db, "fresh", retry_base_seconds=0)
            assert row == ("audio.wav", '{"source_start_ms":0,"source_end_ms":10}')
        finally:
            writer.close()


def test_missing_wal_snapshot_row_is_retried_before_not_found() -> None:
    """A valid-looking DB/WAL pair can cross a checkpoint; retry can recover the committed row."""
    rows = iter([None, ("audio.wav", '{"source_start_ms":0,"source_end_ms":10}')])
    connections = []

    def connect(_path, uri):
        assert uri is True
        connection = _SnapshotConnection(next(rows))
        connections.append(connection)
        return connection

    with (
        mock.patch.object(client.os.path, "exists", return_value=True),
        mock.patch.object(client.shutil, "copyfile", return_value=None),
        mock.patch.object(client.sqlite3, "connect", side_effect=connect),
        mock.patch.object(client.time, "sleep", return_value=None),
    ):
        row = read_segment_from_snapshot("source.db", "seg", attempts=3)

    assert row == ("audio.wav", '{"source_start_ms":0,"source_end_ms":10}')
    assert len(connections) == 2, "row=None with a WAL must not break the retry loop"


def test_stably_missing_wal_snapshot_row_fails_as_unprovable() -> None:
    """Even repeated WAL-backed misses cannot prove absence across a concurrent checkpoint."""
    connections = []

    def connect(_path, uri):
        assert uri is True
        connection = _SnapshotConnection(None)
        connections.append(connection)
        return connection

    with (
        mock.patch.object(client.os.path, "exists", return_value=True),
        mock.patch.object(client.shutil, "copyfile", return_value=None),
        mock.patch.object(client.sqlite3, "connect", side_effect=connect),
        mock.patch.object(client.time, "sleep", return_value=None),
    ):
        try:
            read_segment_from_snapshot("source.db", "missing", attempts=3)
        except SegmentSnapshotError as exc:
            assert "WAL-backed" in str(exc)
        else:
            raise AssertionError("exhausted WAL-backed misses must fail as EX_DB-class uncertainty")
    assert len(connections) == 3


def test_missing_row_without_wal_is_authoritative() -> None:
    """EX_NOSEG remains available only when WAL absence brackets the successful read."""
    connections = []

    def connect(_path, uri):
        assert uri is True
        connection = _SnapshotConnection(None)
        connections.append(connection)
        return connection

    with (
        mock.patch.object(client.os.path, "exists", return_value=False),
        mock.patch.object(client.shutil, "copyfile", return_value=None),
        mock.patch.object(client.sqlite3, "connect", side_effect=connect),
    ):
        row = read_segment_from_snapshot("source.db", "missing", attempts=3)

    assert row is None
    assert len(connections) == 1


def main() -> None:
    test_resolve_clip_offsets()
    test_wal_copy_failure_never_falls_back_to_main_file()
    test_real_wal_snapshot_exposes_committed_segment()
    test_missing_wal_snapshot_row_is_retried_before_not_found()
    test_stably_missing_wal_snapshot_row_fails_as_unprovable()
    test_missing_row_without_wal_is_authoritative()
    print("cortex_7b_client offset + WAL snapshot policy passed")


if __name__ == "__main__":
    main()
