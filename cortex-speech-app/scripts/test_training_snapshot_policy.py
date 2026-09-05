"""A training pack must say what it contains, split without leaking, and seal an immutable snapshot.

Phase 2 of docs/PLAN_TRUE_10.md, from the 2026-08-17 flywheel audit. That audit found the fine-tune
pack emitted "essentially audio, text and duration": a training run could not tell which human
decision produced a label, which recording it came from, or whether the audio was a neural
separator's output rather than an original recording. It was also UNSPLIT — nothing stopped a
fine-tune from validating on a voice it had just trained on, which matters because 94.7 % of today's
labeled duration comes from a single recording.

The governing rule this enforces:

    Record every decision; include every eligible latest human label exactly once in the next
    immutable batch; prove the challenger is better before promotion.

These pins cover the middle clause. Phases 3-4 cover the last one.
"""

from pathlib import Path

from _db_policy_util import database_surface

REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(relative_path: str) -> str:
    if relative_path == "src-tauri/src/db.rs":
        return database_surface(REPO_ROOT / "src-tauri" / "src")
    return (REPO_ROOT / relative_path).read_text(encoding="utf-8")


def test_every_row_carries_its_provenance() -> None:
    eval_rs = _read("src-tauri/src/eval.rs")
    for field in (
        "segment_id",
        "source_recording",
        "split",
        "decision",
        "decision_revision",
        "grade",
        "audio_processed",
    ):
        if f"    {field}:" not in eval_rs:
            raise AssertionError(
                f"FinetuneRow must carry {field!r} — a pack of audio+text+duration cannot be audited, "
                "weighted, or split by anything downstream"
            )


def test_the_split_cannot_leak_a_voice_across_splits() -> None:
    eval_rs = _read("src-tauri/src/eval.rs")
    if "assign_splits" not in eval_rs:
        raise AssertionError(
            "the pack must split with export::assign_splits — clips of one recording have to land in "
            "one split, or a fine-tune validates on the voice it trained on"
        )
    if "SPLIT_SEED" not in eval_rs:
        raise AssertionError(
            "the split seed must be a FIXED constant: the split has to be a property of the data, "
            "not of when the export happened to run"
        )
    # speaker_disjoint = true is the 6th positional arg; pin the call shape rather than the value's
    # spelling so a reordering cannot silently disable it.
    if "SPLIT_SEED, true)" not in eval_rs:
        raise AssertionError("assign_splits must be called with speaker_disjoint = true")

    # The 2026-08-18 leak fix. `assign_splits` groups by the path's BASENAME, which is wrong twice
    # over here: a recording imported under a second name straddles two splits (measured — the
    # Lamofull re-encode landed in `validation` while the same session trained), and the audiobook
    # corpus gives every book its own 01.wav/02.wav so chapters collide across books. Grouping must
    # key on audio CONTENT.
    if "content_identity" not in eval_rs or 'format!("content-{hash}")' not in eval_rs:
        raise AssertionError(
            "the pack must group splits by audio_content_hash, not by file name — basename grouping "
            "put a re-encode of the training material into validation"
        )
    if "a_reencoded_recording_cannot_straddle_two_splits" not in eval_rs:
        raise AssertionError("eval.rs must keep the re-encode leak regression test")

    # And the sealed record must not claim a guarantee the data cannot support.
    if "NOT speaker-disjoint" not in eval_rs:
        raise AssertionError(
            "splitPolicy must state plainly that the split is not speaker-disjoint: diarizer labels "
            "are per-recording indices, not identities, so the speaker union links nothing"
        )


def test_the_snapshot_is_sealed_and_immutable() -> None:
    db_rs = _read("src-tauri/src/db.rs")
    if "pub fn seal_dataset_snapshot" not in db_rs:
        raise AssertionError("db.rs must expose seal_dataset_snapshot")
    if "INSERT OR IGNORE INTO dataset_runs" not in db_rs:
        raise AssertionError(
            "sealing must be INSERT OR IGNORE: a snapshot a training run cites must never be "
            "rewritten underneath it"
        )

    eval_rs = _read("src-tauri/src/eval.rs")
    if "seal_dataset_snapshot(&snapshot_id" not in eval_rs:
        raise AssertionError("the pack export must seal its selection")
    if "let snapshot_id = manifest_sha256.clone();" not in eval_rs:
        raise AssertionError(
            "the snapshot id must BE the manifest content hash, so identical rows always produce the "
            "same id and a different selection can never reuse one"
        )


def test_the_regression_test_exists() -> None:
    eval_rs = _read("src-tauri/src/eval.rs")
    if "finetune_pack_carries_provenance_splits_safely_and_seals_one_snapshot" not in eval_rs:
        raise AssertionError(
            "eval.rs must keep the test proving row provenance, per-recording split containment, "
            "byte-identical re-export, and that an identical selection does not seal twice"
        )


def main() -> None:
    test_every_row_carries_its_provenance()
    test_the_split_cannot_leak_a_voice_across_splits()
    test_the_snapshot_is_sealed_and_immutable()
    test_the_regression_test_exists()
    test_final_pool_outcomes_reach_every_export_boundary()
    test_hf_publication_preserves_managed_generations()
    print("training snapshot policy passed (6 checks)")


def test_final_pool_outcomes_reach_every_export_boundary() -> None:
    expected = {
        "src-tauri/src/export.rs": ("ExportReviewAuthority::retained", "export_review::verify_current", '"review_authority"'),
        "src-tauri/src/export_audio/mod.rs": ("export_review::verify_current", '"review_authority"'),
        "src-tauri/src/export_bundle.rs": ("export_review::verify_current",),
        "src-tauri/src/eval.rs": ("review_authority:", "decision_revision_scope:", "captured_boundary", ".verify_authorities(db, captured_reviews.borrow().iter())"),
        "src-tauri/src/jury/learning.rs": ("LearningReviewScope::capture", "review_scope.includes", "source_example_id:", "review_scope.verify_authorities"),
        "src-tauri/src/export_review.rs": ("struct ExportReviewBoundary", "self.boundary.verify_authorities(db, authorities)", "current.authority(&segment.id) != segment.export_review.as_ref()"),
        "src-tauri/src/jury/mod.rs": ("review_pool::learning_pool", "review_pool::learning_resolutions", "ExportReviewAuthority::retained", "review_authority:", "initial_stamp"),
        "src-tauri/src/review_pool.rs": (
            "shared_export_uses_the_matching_pair_text_not_the_first_opinion",
            "shared_export_excludes_owner_rejection_of_the_first_retained_opinion",
            "shared_export_actual_writers_use_final_text_and_separate_provenance",
            "shared_export_authority_cannot_be_imported_or_reused_after_undo",
            "shared_export_later_retention_does_not_inherit_the_first_rejection",
            "pool_learning_dpo_uses_the_final_matching_pair",
            "pool_learning_lm_uses_the_final_matching_pair",
            "pool_learning_dpo_and_lm_exclude_unresolved_first_opinions",
            "pool_learning_dpo_and_lm_refuse_owner_rejection",
            "pool_learning_later_retention_survives_first_reject_but_not_undo",
            "pool_few_shot_uses_the_final_matching_pair",
            "pool_few_shot_excludes_owner_rejection",
            "pool_few_shot_reuses_only_identity_and_refreshes_retention_and_undo",
            "pool_few_shot_identity_cache_invalidates_external_and_schema_changes",
            "pool_few_shot_final_authority_cannot_bypass_rights_gold_hash_or_unusable_audio",
            "shared_export_pool_activation_invalidates_a_legacy_projection",
            "shared_export_pool_activation_at_pack_publication_preserves_previous_generation",
            "shared_export_pool_activation_invalidates_even_empty_learning_authority",
            "shared_export_publication_boundary_detects_new_duplicate_binding",
        ),
        "scripts/train_challenger.py": ("invalid final-review authority", "canonical_first_opinion"),
    }
    for path, markers in expected.items():
        source = _read(path)
        for marker in markers:
            assert marker in source, f"{path} lost final-review authority guard {marker}"
    db_source = (REPO_ROOT / "src-tauri/src/db.rs").read_text(encoding="utf-8")
    assert '#[serde(skip)]\n    #[specta(skip)]\n    pub export_review:' in db_source, "ordinary IPC/import must exclude export authority"
    assert "export_review: segment.export_review.clone()" in _read("src-tauri/src/export.rs"), "the export-only record must retain final authority"
    assert "shared_export_authority_cannot_be_imported_or_reused_after_undo" in _read("src-tauri/src/review_pool.rs")


def test_hf_publication_preserves_managed_generations() -> None:
    export = _read("src-tauri/src/export.rs")
    publisher = _read("src-tauri/src/hf_publication.rs")
    regressions = _read("src-tauri/src/export_tests.rs")
    assert '"review_authority": {"dtype": "string", "_type": "Value"}' in export, "HF schema must declare serialized review authority as a string"
    assert "HF feature declarations must match every written CSV column" in regressions, "keep generated schema/header parity coverage"
    for marker in ("hf_publication::Publication::begin", "publication.publish(", "write_sha256sums(&staged_root)", "TransactionBehavior::Immediate"):
        assert marker in export, f"HF exporter lost managed-generation publication guard: {marker}"
    assert "std::fs::remove_dir_all(&data_dir)?" not in export, "HF publication must never delete the previous data before promotion"
    for marker in ("lock_destination", "owned_stage", "validate_inventory", "verify_checksums", "journal.json", "COMMITTED", "self.preserve = true", "rollback(&self.root", "recover(&root)", "fsync_directory_strict"):
        assert marker in publisher, f"HF publisher lost recovery authority: {marker}"
    for name in (
        "hf_late_publication_failure_before_data_promotion_preserves_previous_generation",
        "hf_late_publication_failure_before_metadata_write_preserves_previous_generation",
        "hf_late_publication_failure_during_real_split_write_restores_previous_generation",
        "hf_publication_competing_export_is_refused_without_touching_the_first_stage",
        "hf_publication_process_exit_restores_prior_generation_and_releases_lock",
    ):
        assert name in regressions, f"HF exporter lost its behavioral regression: {name}"
    for name in (
        "staged_substitution_is_refused_and_the_previous_generation_is_restored",
        "external_destination_edits_during_staging_are_not_overwritten",
        "unexpected_target_change_preserves_both_the_foreign_bytes_and_the_prior_backup",
        "partial_rollback_can_resume_without_losing_an_already_restored_artifact",
        "corrupted_recovery_journal_is_refused_without_changing_any_artifact",
        "racing_file_after_move_precheck_preserves_both_artifacts",
        "racing_empty_directory_after_move_precheck_preserves_both_artifacts",
    ):
        assert name in publisher, f"HF publisher lost its adversarial regression: {name}"
    assert "rename_no_replace_write_through(source, destination)?" in publisher
    assert "fs::rename(source, destination)?" not in publisher, "an absence precheck cannot make ordinary rename no-clobber"


if __name__ == "__main__":
    main()
