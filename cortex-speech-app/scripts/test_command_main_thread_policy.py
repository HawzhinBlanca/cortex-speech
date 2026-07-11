"""Main-thread-safety gate (audit P0 #2 — auto-discovered by run_python_policies.py).

Tauri runs SYNC `#[tauri::command]`s on the main/UI thread; a slow one there freezes the window
(the same class that caused the Open/Import freeze). Slow commands must be `pub async fn` (dispatched
off the main thread) and offload their blocking body via `run_blocking`/`spawn_blocking`.

This is a RATCHET: the list grows as the migration proceeds, and a command may only be added once it
is genuinely async. Regressing any listed command back to sync fails the gate.
"""
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"

# Slow commands proven moved OFF the main thread. GROW this list as the migration continues; never
# shrink it (that would be a UI-freeze regression).
ASYNC_SLOW_COMMANDS = [
    "open_audio_file",  # non-blocking native picker (crash fix f01ab66)
    "import_directory",  # non-blocking native folder picker (crash fix f01ab66)
    # Export family — DB scan + serialize + hash + (re)encode + atomic write, all via run_blocking.
    "export_dataset",
    "export_transcript",
    "export_huggingface_dataset",
    "export_dataset_bundle",
    "export_audio",
    "export_gold_eval_set",
    "export_finetune_pack",
    # Dataset-wide DB scans / file-I/O reads + writes (whole-library work that froze the UI).
    "get_segments",
    "get_segments_suspect_first",
    "get_dataset_stats",
    "get_intelligence_report",
    "get_audio_health",
    "relink_audio",
    "db_vacuum",
    "merge_dataset_json",
    # Eval / quality / calibration compute over the whole dataset + a model-integrity hash.
    "get_dataset_certificate",
    "run_gold_eval",
    "compute_annotation_drift_scorecard",
    "get_label_quality_lift",
    "validate_dataset_cmd",
    "get_dataset_quality",
    "verify_finetuned_model_integrity",
    # Active-learning compute + pipeline-clone ASR/decode reads (audio decode + ONNX inference).
    "get_active_learning_queue",
    "get_waveform",
    "transcribe_segment_constrained",
    "rediarize_segments",
    "run_gold_eval_asr",
    "run_gold_eval_local",
    # Per-segment ASR/alignment (decode + ONNX/WSL inference, some with a brief db write).
    "transcribe_segment",
    "transcribe_segment_finetuned",
    "align_segment",
    "check_audio",
    # Jury gate + gold imports + model-checkpoint hash (audio reads / DB writes / multi-GB SHA-256).
    "run_t0_gate",
    "import_gold_segments",
    "create_gold_from_file",
    "import_verified_segments_as_gold",
    "import_model_checkpoint",
]

# Commands whose blocking body must run on the spawn_blocking pool (not inline on a tokio worker).
RUN_BLOCKING_COMMANDS = [
    "export_dataset",
    "export_transcript",
    "export_huggingface_dataset",
    "export_dataset_bundle",
    "export_audio",
    "export_gold_eval_set",
    "export_finetune_pack",
    "get_segments",
    "get_segments_suspect_first",
    "get_dataset_stats",
    "get_intelligence_report",
    "get_audio_health",
    "relink_audio",
    "db_vacuum",
    "merge_dataset_json",
    "get_active_learning_queue",
]


def source() -> str:
    return COMMANDS_RS.read_text(encoding="utf-8")


def test_listed_slow_commands_are_async() -> None:
    src = source()
    for name in ASYNC_SLOW_COMMANDS:
        if f"pub async fn {name}(" not in src:
            raise AssertionError(
                f"command `{name}` must be `pub async fn` (off the main thread) — found sync or missing. "
                "A slow sync command runs on the UI thread and freezes the window."
            )


def test_off_main_thread_helper_exists_and_is_used() -> None:
    src = source()
    if "async fn run_blocking" not in src:
        raise AssertionError("run_blocking helper (spawn_blocking wrapper) is missing from commands.rs")
    if "spawn_blocking" not in src:
        raise AssertionError("commands.rs must offload blocking work via tokio spawn_blocking")
    uses = src.count("run_blocking(move ||")
    if uses < len(RUN_BLOCKING_COMMANDS):
        raise AssertionError(
            f"expected >= {len(RUN_BLOCKING_COMMANDS)} run_blocking call sites (the migrated exports), found {uses}"
        )


def test_migrated_exports_do_not_hold_lock_db_across_the_await() -> None:
    # The blocking body must clone the Arc handle (db_arc) and lock INSIDE the task — never take a
    # `lock_db()` guard and carry it across the await (a non-Send guard across await won't compile,
    # but this pins the intended pattern so a future edit doesn't reintroduce a main-thread lock).
    src = source()
    for name in RUN_BLOCKING_COMMANDS:
        start = src.index(f"pub async fn {name}(")
        body = src[start : start + 1200]
        if "state.db_arc()" not in body:
            raise AssertionError(f"`{name}` must obtain the DB via state.db_arc() for the blocking task")


def main() -> None:
    test_listed_slow_commands_are_async()
    test_off_main_thread_helper_exists_and_is_used()
    test_migrated_exports_do_not_hold_lock_db_across_the_await()
    print("command main-thread policy regression passed")


if __name__ == "__main__":
    main()
