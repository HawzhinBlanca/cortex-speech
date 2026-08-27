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
        "database.get_segments_by_ids(ids)",
        "database.delete_segments_batch(ids)",
        "Command::DeleteSegments",
        "fn rename_speaker_v1(",
        "begin_mutation().map_err(AppError::Other).map_err(SpeakerRenameError::from)?",
        ".rename_speaker_with_inventory(old_id, new_id, expected_source_count, expected_target_count)",
        "SpeakerRenameError::Stale { source_count, target_count }",
        "fn assign_speaker_batch_v1(",
        ".assign_speaker_batch_atomic(ids, target_speaker_id)",
        "Command::SpeakerAssignment { changes }",
    ):
        if required not in store:
            raise AssertionError(f"SegmentWriteStore lost required mutation boundary: {required}")


def test_migrated_commands_validate_then_delegate_without_raw_database_authority() -> None:
    source = read("commands/segments_write.rs")
    signatures = {
        "update_segment_metadata_v1": "pub fn update_segment_metadata_v1(",
        "delete_segments_v1": "pub fn delete_segments_v1(",
        "rename_speaker_v1": "pub async fn rename_speaker_v1(",
    }
    for name, signature in signatures.items():
        body = command(source, signature)
        if ".segment_writes()" not in body:
            raise AssertionError(f"{name} bypasses SegmentWriteStore")
        for forbidden in ("state.lock_db()", "state.db_arc()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")

    for name in ("update_segment_metadata_v1", "delete_segments_v1", "rename_speaker_v1"):
        body = command(source, signatures[name])
        if "_mutation" not in body:
            raise AssertionError(f"{name} no longer retains the restore-admission token")
        session_save = "app_state.session_auto_save()" if name == "rename_speaker_v1" else "state.session_auto_save()"
        if session_save not in body:
            raise AssertionError(f"{name} no longer keeps restore admission alive through session save")
    for name in ("update_segment_metadata_v1", "delete_segments_v1"):
        if "validate::validate_identifier" not in command(source, signatures[name]):
            raise AssertionError(f"{name} lost identifier validation")
    rename = command(source, signatures["rename_speaker_v1"])
    for required in (
        "validate::validate_text(source_speaker_id, 256",
        "validate::validate_speaker_label(&request.target_speaker_id)",
        "public_speaker_rename_error",
        "expected_source_count",
        "expected_target_count",
        "spawn_blocking",
        "app.try_state::<AppState>()",
    ):
        if required not in rename:
            raise AssertionError(f"typed speaker rename boundary lost {required!r}")

    database = read("db/queries_recovery.rs")
    rename_start = database.find("pub fn rename_speaker_with_inventory(")
    rename_end = database.find("pub fn speaker_counts(", rename_start)
    if min(rename_start, rename_end) < 0:
        raise AssertionError("atomic database speaker rename boundary is missing")
    rename_sql = database[rename_start:rename_end]
    for required in (
        "expected_source_count",
        "expected_target_count",
        "SAVEPOINT speaker_rename",
        "assign_speaker_batch_atomic",
        "source_after != 0",
    ):
        if required not in rename_sql:
            raise AssertionError(f"speaker rename lost atomic inventory guard {required!r}")

    batch = read("commands/batch.rs")
    batch_body = command(batch, "pub async fn assign_speakers_v1(")
    for required in (
        "#[specta::specta]",
        "validate::validate_identifier",
        "validate::validate_speaker_label",
        "spawn_blocking",
        ".segment_writes()",
        ".assign_speaker_batch_v1",
        "_mutation",
        "session_auto_save()",
        "public_speaker_assignment_error",
    ):
        if required not in batch_body and required != "#[specta::specta]":
            raise AssertionError(f"typed batch speaker assignment lost {required!r}")
    if "#[specta::specta]\npub async fn assign_speakers_v1(" not in batch:
        raise AssertionError("batch speaker assignment is not in the generated IPC registry")
    for forbidden in ("state.lock_db()", "state.db_arc()", ".connection()", "thread::spawn"):
        if forbidden in batch_body:
            raise AssertionError(f"batch speaker assignment bypasses its store/worker boundary: {forbidden}")

    history = read("history/mod.rs")
    for required in (
        "SpeakerAssignment {",
        "db.apply_speaker_assignment_history(changes, false)?",
        "db.apply_speaker_assignment_history(changes, true)?",
        "MAX_HISTORY_BYTES",
        "retained_bytes > self.max_bytes",
    ):
        if required not in history:
            raise AssertionError(f"exact speaker undo/redo lost {required!r}")

    system_ops = read("commands/system_ops.rs")
    for action in ("undo", "redo"):
        body = command(system_ops, f"pub fn {action}(")
        for required in ("begin_mutation()", "_mutation", "state.session_auto_save()"):
            if required not in body:
                raise AssertionError(f"{action} no longer holds restore admission through session save: {required}")

    deletion = command(source, signatures["delete_segments_v1"])
    for required in ("MAX_SEGMENT_DELETE_IDS", "public_segment_delete_error", "deleted_count > 0"):
        if required not in deletion:
            raise AssertionError(f"typed deletion boundary lost {required!r}")


def test_duplicate_batch_ids_fail_before_shared_evidence_archival() -> None:
    source = read("db/segments.rs")
    start = source.find("pub fn delete_segments_batch(")
    end = source.find("pub fn get_segment_by_id(", start)
    if start < 0 or end < 0:
        raise AssertionError("shared batch deletion boundary not found")
    body = source[start:end]
    duplicate_guard = body.find("ids.iter().any(|id| !unique_ids.insert(id.as_str()))")
    savepoint = body.find('self.conn.execute("SAVEPOINT batch_delete", [])?')
    archival = body.find("self.archive_loop0_evidence_for(id)?")
    if min(duplicate_guard, savepoint, archival) < 0 or not duplicate_guard < savepoint < archival:
        raise AssertionError("duplicate ids are not refused before batch savepoint and evidence archival")

    runtime_tests = read("db_tests.rs")
    for required in (
        "fn duplicate_batch_ids_cannot_double_archive_loop0_or_c4_evidence()",
        "assert_eq!(loop0_after, loop0_before",
        "assert_eq!(c4_after, c4_before",
    ):
        if required not in runtime_tests:
            raise AssertionError(f"duplicate-id evidence regression lost runtime proof {required!r}")


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
    test_duplicate_batch_ids_fail_before_shared_evidence_archival()
    test_retired_whole_row_command_cannot_acquire_database_authority()
    print("segment-write store architecture policy passed")


if __name__ == "__main__":
    main()
