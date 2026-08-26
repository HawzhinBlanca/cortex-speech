"""Architecture policy for the first query-store strangler slice."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "src"


def _read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def _fn(source: str, signature: str, span: int = 2200) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing architecture boundary `{signature}`")
    return source[start : start + span]


def _command(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing command boundary `{signature}`")
    end = source.find("\n#[tauri::command]", start + len(signature))
    return source[start:] if end < 0 else source[start:end]


def test_store_is_backend_only_and_connection_bounded() -> None:
    store = _read("stores/segment_query.rs")
    for forbidden in ("tauri", "crate::http", "crate::commands", "rusqlite::Connection"):
        if forbidden in store:
            raise AssertionError(f"SegmentQueryStore crossed a forbidden layer: {forbidden}")
    required = (
        "struct SegmentQueryStore",
        "runtime: DatabaseRuntime",
        "self.runtime.open_read()?",
        "get_segments_page",
        "active_learning_queue",
        "resolve_transcription_segment",
    )
    for needle in required:
        if needle not in store:
            raise AssertionError(f"SegmentQueryStore lost required boundary: {needle}")


def test_bounded_readers_do_not_take_the_writer_mutex_for_the_live_path() -> None:
    runtime = _read("database_runtime.rs")
    if "database_path: Arc<str>" not in runtime:
        raise AssertionError("DatabaseRuntime must own the immutable live database path")
    body = _fn(runtime, "pub(crate) fn open_read(", 1500)
    if "self.database_path.as_ref()" not in body:
        raise AssertionError("bounded reads must open the runtime-owned live database path")
    if "self.lock()" in body or "self.writer.lock" in body:
        raise AssertionError("bounded query snapshots regressed to contending on the serialized writer mutex")
    if "self.reads.acquire()?" not in body or "self.admission.begin_capture()" not in body:
        raise AssertionError("bounded query snapshots must hold capacity and restore admission")


def test_migrated_command_handlers_use_only_the_query_store() -> None:
    segments = _read("commands/segments_read.rs")
    for name in (
        "get_review_page_v1",
        "get_segments",
        "get_segments_suspect_first",
        "search_segments",
        "get_audio_health",
        "get_active_learning_queue",
    ):
        body = _command(segments, f"pub async fn {name}(")
        if "segment_queries" not in body:
            raise AssertionError(f"{name} bypasses SegmentQueryStore")
        if "state.db_arc()" in body or "state.lock_db()" in body or "state.db_runtime()" in body:
            raise AssertionError(f"{name} regained raw database authority")

    commands = _read("commands.rs")
    for name in ("get_segment", "get_segments_page", "get_segment_ids_for_view", "get_signal_anomaly_segments"):
        body = _command(commands, f"pub fn {name}(")
        if ".segment_queries()" not in body:
            raise AssertionError(f"{name} bypasses SegmentQueryStore")
        if "state.lock_db()" in body or "state.db_arc()" in body:
            raise AssertionError(f"{name} regained raw database authority")


def main() -> None:
    test_store_is_backend_only_and_connection_bounded()
    test_bounded_readers_do_not_take_the_writer_mutex_for_the_live_path()
    test_migrated_command_handlers_use_only_the_query_store()
    print("segment-query store architecture policy passed")


if __name__ == "__main__":
    main()
