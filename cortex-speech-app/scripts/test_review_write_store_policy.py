"""Architecture policy for the serialized desktop human-review write boundary."""

from pathlib import Path

from _db_policy_util import database_surface


REPO = Path(__file__).resolve().parents[1]
RUST = REPO / "src-tauri" / "src"


def read(relative: str) -> str:
    if relative == "db.rs":
        return database_surface(RUST)
    return (RUST / relative).read_text(encoding="utf-8")


def command(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing command boundary `{signature}`")
    candidates = [
        boundary
        for boundary in (
            source.find("\n#[tauri::command]", start + len(signature)),
            source.find("\n#[cfg(test)]", start + len(signature)),
        )
        if boundary >= 0
    ]
    end = min(candidates) if candidates else len(source)
    return source[start:end]


def test_store_is_backend_only_and_serializes_each_effect_write() -> None:
    store = read("stores/review_write.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http", "rusqlite"):
        if forbidden in store.split("#[cfg(test)]", 1)[0]:
            raise AssertionError(f"ReviewWriteStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct ReviewWriteStore",
        "runtime: DatabaseRuntime",
        "self.runtime.lock()",
        "commit_legacy_decision",
        "commit_typed_decision",
        "replay_desktop_human_decision",
        "replay_desktop_review_v1_and_clear_draft",
        "finalize_human_review_with_playback",
        "finalize_desktop_review_v1_with_playback",
        "has_sufficient_playback_evidence",
        "desktop_review_undo_availability",
        "undo_latest_desktop_human_decision",
        "record_review_flag",
        "undo_latest_desktop_review_flag",
        "clear_human_decision",
    ):
        if required not in store:
            raise AssertionError(f"ReviewWriteStore lost required boundary: {required}")


def test_migrated_commands_validate_then_delegate_without_raw_database_authority() -> None:
    # `#[cfg(test)]` is also used on individual characterization helpers before later production
    # commands, so truncating at the first occurrence can silently stop auditing the real surface.
    # Extract each exact `pub fn` command body from the complete file instead.
    commands = read("commands/segments_write.rs")
    expectations = {
        "commit_review_v1": "commit_review_v1_on_with_source_lease_at_generation(",
        "record_review_flag": "record_review_flag_on(",
        "clear_human_decision": ".clear_legacy_decision(",
    }
    for name, delegation in expectations.items():
        body = command(commands, f"pub fn {name}(")
        if ".review_writes()" not in body or delegation not in body:
            raise AssertionError(f"{name} bypasses ReviewWriteStore")
        for forbidden in ("state.lock_db()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")

    retired_undo = command(commands, "pub fn undo_human_decision(")
    if "TYPED_UNDO_REQUIRED" not in retired_undo or ".review_writes()" in retired_undo:
        raise AssertionError("retired undo_human_decision must fail closed without a mutation path")
    if "pub fn undo_review_flag(" in commands:
        raise AssertionError("identity-light undo_review_flag must not remain a production command")

    typed_undo = command(commands, "fn undo_desktop_review_action_v1_on(")
    if (
        "ReviewWriteStore" not in typed_undo
        or ".undo_latest_desktop_human_decision(" not in typed_undo
        or ".undo_latest_desktop_review_flag(" not in typed_undo
        or "begin_mutation_at_restore_generation_serial" not in typed_undo
        or "DesktopReviewUndoTargetV1::Decision" not in typed_undo
        or "DesktopReviewUndoTargetV1::Flag" not in typed_undo
        or "crate::db::Database" in typed_undo
    ):
        raise AssertionError("typed desktop Undo bypasses ReviewWriteStore exact-target authority")

    typed_target = command(commands, "fn get_desktop_review_undo_target_v1_on(")
    if (
        "ReviewWriteStore" not in typed_target
        or ".desktop_review_undo_availability()" not in typed_target
        or "crate::db::Database" in typed_target
    ):
        raise AssertionError("typed desktop Undo discovery bypasses ReviewWriteStore authority")

    retired = command(commands, "pub fn record_human_decision(")
    for required in ("retired_legacy_decision_error()", "TYPED_REVIEW_REQUIRED"):
        if required not in retired and required not in commands:
            raise AssertionError(f"retired record_human_decision lost its fail-closed marker: {required}")
    if ".review_writes()" in retired or "record_human_decision_on(" in retired:
        raise AssertionError("retired record_human_decision must not retain a mutation path")

    flag = command(commands, "pub fn record_review_flag(")
    for required in (
        "RecordReviewFlagRequestV1",
        "record_review_flag_on(&state.review_writes(), &request)",
        "Result<RecordedReviewFlagV1, CommandErrorV1>",
    ):
        if required not in flag:
            raise AssertionError(f"record_review_flag lost command-layer validation: {required}")

    flag_adapter = command(commands, "fn record_review_flag_on(")
    for required in (
        "validate::validate_identifier(&request.segment_id)",
        "validate::validate_text(&request.rationale",
        "request.base_revision < 0",
        "ReviewFlagCommitError::StaleRevision",
        '"STALE_REVISION"',
        '.detail("expectedRevision", request.base_revision)',
        '.detail("currentRevision", current_revision)',
    ):
        if required not in flag_adapter:
            raise AssertionError(f"record_review_flag lost typed CAS authority: {required}")

    for helper in ("fn record_human_decision_on(", "fn commit_review_v1_on("):
        start = commands.find(helper)
        if start < 0:
            raise AssertionError(f"missing review command adapter: {helper}")
        body = commands[start : commands.find("\n}", start) + 2]
        if "ReviewWriteStore" not in body or "crate::db::Database" in body:
            raise AssertionError(f"{helper} does not stay on the review-store boundary")


def test_identity_free_clear_stays_retired_and_undo_stays_effect_bound() -> None:
    database = read("db.rs")
    for required in (
        "clear_human_decision is disabled: undo requires an immutable decision effect id and operation UUID",
        "pub fn undo_human_decision(",
        "human_decision_effect_reversals",
        "pub fn undo_review_flag(",
        "review_flag_effect_reversals",
    ):
        if required not in database:
            raise AssertionError(f"review-effect authority regressed: {required}")


def main() -> None:
    test_store_is_backend_only_and_serializes_each_effect_write()
    test_migrated_commands_validate_then_delegate_without_raw_database_authority()
    test_identity_free_clear_stays_retired_and_undo_stays_effect_bound()
    print("review-write store architecture policy passed")


if __name__ == "__main__":
    main()
