"""Architecture policy for the serialized desktop human-review write boundary."""

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
        "undo_human_decision",
        "record_review_flag",
        "undo_review_flag",
        "clear_human_decision",
    ):
        if required not in store:
            raise AssertionError(f"ReviewWriteStore lost required boundary: {required}")


def test_migrated_commands_validate_then_delegate_without_raw_database_authority() -> None:
    commands = read("commands/segments_write.rs").split("#[cfg(test)]", 1)[0]
    expectations = {
        "record_human_decision": "record_human_decision_on(",
        "commit_review_v1": "commit_review_v1_on(",
        "undo_human_decision": ".undo_human_decision(",
        "record_review_flag": ".record_flag(",
        "undo_review_flag": ".undo_flag(",
        "clear_human_decision": ".clear_legacy_decision(",
    }
    for name, delegation in expectations.items():
        body = command(commands, f"pub fn {name}(")
        if ".review_writes()" not in body or delegation not in body:
            raise AssertionError(f"{name} bypasses ReviewWriteStore")
        for forbidden in ("state.lock_db()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")

    flag = command(commands, "pub fn record_review_flag(")
    for required in ("validate::validate_identifier(&segment_id)", "validate::validate_text(&rationale"):
        if required not in flag:
            raise AssertionError(f"record_review_flag lost command-layer validation: {required}")

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
