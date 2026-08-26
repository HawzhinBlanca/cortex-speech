"""Non-authoritative crash-safe review-draft architecture gate."""

from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
RUST = REPO / "src-tauri" / "src"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def test_schema_is_one_additive_post_v65_layer() -> None:
    migrations = read(RUST / "migrations" / "mod.rs")
    marker = "version: 66,"
    if migrations.count(marker) != 1:
        raise AssertionError("review drafts require exactly one additive schema-v66 migration")
    v66 = migrations[migrations.index(marker) :]
    for required in (
        "CREATE TABLE review_drafts",
        "segment_id   TEXT PRIMARY KEY",
        "base_revision INTEGER NOT NULL CHECK(base_revision >= 0)",
        "FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE",
        ") STRICT;",
        "DROP TABLE review_drafts",
    ):
        if required not in v66:
            raise AssertionError(f"schema-v66 lost the review-draft invariant: {required}")


def test_store_is_tauri_free_revision_bound_and_transactional() -> None:
    store = read(RUST / "stores" / "review_draft.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"ReviewDraftStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct ReviewDraftStore",
        "TransactionBehavior::Immediate",
        "database.with_full_sync",
        "SELECT review_revision FROM speech_segments",
        "E_STALE_REVIEW_DRAFT",
        "ON CONFLICT(segment_id) DO UPDATE",
        "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
    ):
        if required not in store:
            raise AssertionError(f"ReviewDraftStore lost required safety: {required}")


def test_commands_use_store_and_generated_contract() -> None:
    commands = read(RUST / "commands" / "segments_write.rs")
    production = commands.split("#[cfg(test)]", 1)[0]
    for name in ("get_review_draft_v1", "save_review_draft_v1", "delete_review_draft_v1"):
        start = production.find(f"pub fn {name}(")
        if start < 0:
            raise AssertionError(f"missing typed draft command {name}")
        end = production.find("\n#[tauri::command]", start + 1)
        body = production[start:] if end < 0 else production[start:end]
        if ".review_drafts()" not in body:
            raise AssertionError(f"{name} bypasses ReviewDraftStore")
        if "state.lock_db()" in body or "review_drafts " in body:
            raise AssertionError(f"{name} regained raw SQL/database authority")

    contract = read(RUST / "ipc_contract.rs")
    registry = read(RUST / "lib.rs")
    generated = read(REPO / "src" / "lib" / "generated" / "ipc.ts")
    for name in ("get_review_draft_v1", "save_review_draft_v1", "delete_review_draft_v1"):
        if f"crate::commands::{name}" not in contract or f"commands::{name}" not in registry:
            raise AssertionError(f"{name} is absent from a Rust command registry")
    for name in ("getReviewDraftV1", "saveReviewDraftV1", "deleteReviewDraftV1"):
        if name not in generated:
            raise AssertionError(f"generated TypeScript bindings lost {name}")


def test_decision_clear_is_exact_atomic_and_replay_safe() -> None:
    database = read(RUST / "db.rs")
    for required in (
        "replay_desktop_review_v1_and_clear_draft",
        "review_draft_revision: Option<i64>",
        "expected_revision == review_draft_revision",
        "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
        "Some(base_revision)",
    ):
        if required not in database:
            raise AssertionError(f"typed decision/draft atomicity lost: {required}")
    if database.count("DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2") < 3:
        raise AssertionError(
            "draft deletion must remain present in replay preflight, replay race, and first commit; "
            "other exact-revision terminal workflows may use the same safe deletion"
        )


def test_drafts_never_enter_truth_export_eval_payment_or_serving_queries() -> None:
    authority_paths = [
        RUST / "export.rs",
        RUST / "export_bundle.rs",
        RUST / "transcript_export.rs",
        RUST / "eval.rs",
        RUST / "stats.rs",
        RUST / "quality.rs",
        RUST / "review_pool.rs",
        RUST / "review_pool_export.rs",
        RUST / "couch.rs",
    ]
    contaminated = [str(path.relative_to(REPO)) for path in authority_paths if "review_drafts" in read(path)]
    if contaminated:
        raise AssertionError(f"non-authoritative drafts entered an authority query: {contaminated}")


def test_frontend_recovers_without_automatic_merge_or_direct_tauri_access() -> None:
    review = read(REPO / "src" / "lib" / "ReviewMode.svelte")
    for required in (
        "api.getReviewDraftV1",
        "api.saveReviewDraftV1",
        "api.deleteReviewDraftV1",
        "draft.baseRevision === baseRevision",
        "draftConflict = draft",
        "Never merge human text automatically",
        "ReviewDraftWriteCoordinator",
        "draftWrites.flushAll()",
    ):
        if required not in review:
            raise AssertionError(f"ReviewMode lost draft-recovery behavior: {required}")
    if "@tauri-apps" in review or "generatedCommands" in review:
        raise AssertionError("ReviewMode must call domain command adapters, never Tauri/generated IPC directly")
    coordinator = read(REPO / "src" / "lib" / "reviewDraftWriteCoordinator.ts")
    for required in (
        "private readonly desired",
        "saved.segmentId !== intent.segmentId",
        "saved.baseRevision !== intent.baseRevision",
        "saved.text !== intent.text",
        "Promise.allSettled",
    ):
        if required not in coordinator:
            raise AssertionError(f"draft write coordinator lost fail-closed durability behavior: {required}")


def main() -> None:
    test_schema_is_one_additive_post_v65_layer()
    test_store_is_tauri_free_revision_bound_and_transactional()
    test_commands_use_store_and_generated_contract()
    test_decision_clear_is_exact_atomic_and_replay_safe()
    test_drafts_never_enter_truth_export_eval_payment_or_serving_queries()
    test_frontend_recovers_without_automatic_merge_or_direct_tauri_access()
    print("review-draft non-authority architecture policy passed")


if __name__ == "__main__":
    main()
