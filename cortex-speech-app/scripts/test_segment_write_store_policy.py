"""Architecture policy for the segment deletion/rename store slice."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "src"


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def command(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing command boundary `{signature}`")
    end = source.find("\n#[tauri::command]", start + len(signature))
    return source[start:] if end < 0 else source[start:end]


def test_store_owns_serialized_deletes_history_and_rename_without_ui_dependencies() -> None:
    store = read("stores/segment_write.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"SegmentWriteStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct SegmentWriteStore",
        "runtime: DatabaseRuntime",
        "history: Arc<Mutex<HistoryManager>>",
        "struct SegmentMutation",
        "_admission: MutationGuard<'static>",
        "begin_mutation().map_err(AppError::Other)?",
        "fn update_metadata_v1(",
        "SegmentMetadataChange::SpeakerId",
        "SegmentMetadataChange::AlignmentJson",
        "segment.speaker_id != *expected && segment.speaker_id != *value",
        "segment.alignment_json != *expected && segment.alignment_json != *value",
        "HistoryManager::persist_segment_update(&database, &history, &segment)?",
        "database.get_segment_by_id(id)?",
        "database.get_segments_by_ids(ids)?",
        "database.delete_segment(id)?",
        "database.delete_segments_batch(ids)?",
        "Command::DeleteSegments",
        '.rename_speaker(old_id, new_id)',
    ):
        if required not in store:
            raise AssertionError(f"SegmentWriteStore lost required mutation boundary: {required}")


def test_migrated_commands_validate_then_delegate_without_raw_database_authority() -> None:
    source = read("commands/segments_write.rs")
    signatures = {
        "update_segment_metadata_v1": "pub fn update_segment_metadata_v1(",
        "delete_segment": "pub fn delete_segment(",
        "delete_segments_batch": "pub fn delete_segments_batch(",
        "rename_speaker": "pub fn rename_speaker(",
    }
    for name, signature in signatures.items():
        body = command(source, signature)
        if ".segment_writes()" not in body:
            raise AssertionError(f"{name} bypasses SegmentWriteStore")
        for forbidden in ("state.lock_db()", "state.db_arc()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")

    for name in ("update_segment_metadata_v1", "delete_segment", "delete_segments_batch"):
        body = command(source, signatures[name])
        if "validate::validate_identifier" not in body:
            raise AssertionError(f"{name} lost identifier validation")
        if "_mutation" not in body:
            raise AssertionError(f"{name} no longer retains the restore-admission token")
        if "state.session_auto_save()" not in body:
            raise AssertionError(f"{name} no longer keeps restore admission alive through session save")
    if "validate::validate_identifier(&new_id)" not in command(source, signatures["rename_speaker"]):
        raise AssertionError("rename_speaker lost new identity validation")


def test_retired_whole_row_command_cannot_acquire_database_authority() -> None:
    body = command(read("commands/segments_write.rs"), "pub fn update_segment(")
    if "WHOLE_ROW_SEGMENT_WRITE_RETIRED" not in body:
        raise AssertionError("retired whole-row command lost its explicit refusal")
    for forbidden in ("state.lock_db()", "state.db_arc()", ".segment_writes()", ".connection()"):
        if forbidden in body:
            raise AssertionError(f"retired update_segment regained authority: {forbidden}")


def main() -> None:
    test_store_owns_serialized_deletes_history_and_rename_without_ui_dependencies()
    test_migrated_commands_validate_then_delegate_without_raw_database_authority()
    test_retired_whole_row_command_cannot_acquire_database_authority()
    print("segment-write store architecture policy passed")


if __name__ == "__main__":
    main()
