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

REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(relative_path: str) -> str:
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
    print("training snapshot policy passed (4 assertions)")


if __name__ == "__main__":
    main()
