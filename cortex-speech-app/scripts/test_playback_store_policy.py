"""Architecture policy for the serialized playback-write strangler slice."""

from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
RUST = REPO / "src-tauri" / "src"


def read(relative: str) -> str:
    return (RUST / relative).read_text(encoding="utf-8")


def command(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing command boundary `{signature}`")
    end = source.find("\n#[tauri::command]", start + len(signature))
    return source[start:] if end < 0 else source[start:end]


def test_store_is_backend_only_and_serialized() -> None:
    store = read("stores/playback.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"PlaybackWriteStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct PlaybackObservation",
        "struct PlaybackWriteStore",
        "self.runtime.lock()",
        "database.record_playback_observation",
    ):
        if required not in store:
            raise AssertionError(f"PlaybackWriteStore lost required authority boundary: {required}")


def test_command_validates_then_delegates_without_raw_database_authority() -> None:
    commands = read("commands/segments_write.rs")
    body = command(commands, "pub fn record_playback_receipt(")
    for required in (
        'RATE_LIMITER.check("record_playback_receipt")',
        "validate::validate_identifier(&segment_id)",
        "validate_playback_receipt_identity",
        "played_ms < 0 || clip_duration_ms < 0",
        "PLAYBACK_SESSION_REQUIRED",
    ):
        if required not in body:
            raise AssertionError(f"retired scalar playback command lost its fail-closed contract: {required}")
    for forbidden in (
        "state.lock_db()",
        ".playback_writes()",
        ".record_observation(PlaybackObservation",
        "segment_audio_content_hash",
        "segment_review_revision",
        "crate::db::PlaybackReceipt",
    ):
        if forbidden in body:
            raise AssertionError(f"playback command regained database authority: {forbidden}")


def test_database_remains_final_identity_and_coverage_authority() -> None:
    database = read("db.rs")
    for required in (
        "pub(crate) struct PlaybackReceiptObservation",
        "pub(crate) fn record_playback_observation",
    ):
        if required not in database:
            raise AssertionError(f"database lost observation-only playback API: {required}")
    start = database.find("pub fn record_playback_receipt(&self")
    if start < 0:
        raise AssertionError("missing canonical database playback writer")
    body = database[start : start + 4200]
    for required in (
        "COALESCE(review_revision, 0)",
        "audio_content_hash",
        "COALESCE(duration_ms, 0)",
        "canonical_source_span",
        "segment_revision: revision",
        "clip_duration_ms: duration_ms",
        "self.record_playback_receipt_raw(&resolved)",
    ):
        if required not in body:
            raise AssertionError(f"database lost server-owned playback identity: {required}")


def main() -> None:
    test_store_is_backend_only_and_serialized()
    test_command_validates_then_delegates_without_raw_database_authority()
    test_database_remains_final_identity_and_coverage_authority()
    print("playback-write store architecture policy passed")


if __name__ == "__main__":
    main()
