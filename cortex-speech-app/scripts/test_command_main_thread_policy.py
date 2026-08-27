"""Main-thread-safety gate (audit P0 #2 — auto-discovered by run_python_policies.py).

Tauri runs SYNC `#[tauri::command]`s on the main/UI thread; a slow one there freezes the window
(the same class that caused the Open/Import freeze). Slow commands must be `pub async fn` (dispatched
off the main thread) and offload their blocking body via `run_blocking`/`spawn_blocking`.

This is a RATCHET: the list grows as the migration proceeds, and a command may only be added once it
is genuinely async. Regressing any listed command back to sync fails the gate. A deliberately retired
IPC leaves the list only when a separate release-surface policy proves that command remains absent.

READ THIS BEFORE DELETING ANY COMMAND NAMED BELOW (iteration 233). A dead-code audit found 17 IPC
commands with no frontend `invoke(...)` caller and proposed cutting all of them — ~597 lines. That was
WRONG for 11 of the 17, and this file is why: the lists here PIN command names, and the gate asserts each
one exists and is async. Deleting such a command either fails this gate or must be paired with an
explicit source-absence contract. Trimming the list without that proof silently weakens the ratchet.

So "no frontend caller" does NOT mean "dead" in this repo. A command can be load-bearing for a gate, for
`test_ui_thread_blocking_audit.py`'s freezer worklist, or as the sole caller of real logic. Before cutting
any command, search the WHOLE repo with no path filter — scripts/, src-tauri/fuzz/, .github/, docs/ —
and let the compiler and these gates, not a grep over src/, decide what is reachable. A grep proves
presence, never absence.

(ponytail-audit round 2, 2026-08-13: `run_consensus_refinery` and `compute_annotation_drift_scorecard`
were removed from ASYNC_SLOW_COMMANDS below after full-repo verification found zero callers anywhere —
frontend, scripts, fuzz, CI, docs — beyond this ratchet list itself and their own `generate_handler!`
registration. `get_blocking_validation_issues` (a sync command, never in this async list) was cut the
same way; its sole callee `export_bundle::blocking_issues` was dead code once the wrapper went, so it
was cut too — the struct it returned, `BlockingValidationIssues`, is still built inline by the live
`export_dataset_bundle` and was NOT touched.)
"""
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"
# Week-4 decomposes commands.rs into slices under src/commands/ (one slice per run). The command
# SURFACE is therefore commands.rs PLUS its slices — this gate must follow a command wherever it
# lives, or it would silently stop checking it the moment it moved, which is a vacuous pass.
COMMANDS_DIR = REPO_ROOT / "src-tauri" / "src" / "commands"

# Slow commands proven moved OFF the main thread. Grow this list as the migration continues. A command
# leaves it only when the command itself is deliberately removed from the shipped IPC surface (with an
# explicit absence policy), never merely to silence a main-thread regression.
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
    "search_segments",  # unbounded FTS5 MATCH (no LIMIT) — off the main thread like its get_segments siblings
    "get_dataset_stats",
    "get_intelligence_report",
    "get_audio_health",
    "relink_audio",
    "db_vacuum",
    "merge_dataset_json",
    "assign_speakers_v1",
    "rename_speaker_v1",
    # Eval / quality / calibration compute over the whole dataset.
    "get_dataset_certificate",
    "run_gold_eval",
    "get_label_quality_lift",
    "validate_dataset_cmd",
    "get_dataset_quality",
    # Active-learning compute + pipeline-clone ASR/decode reads (audio decode + ONNX inference).
    "get_active_learning_queue",
    "get_waveform",
    "rediarize_segments",
    "run_gold_eval_asr",
    # Per-segment ASR/alignment (decode + ONNX/WSL inference, some with a brief db write).
    "transcribe_segment",
    "align_segment",
    "check_audio",
    # Jury gate + gold imports + model-checkpoint hash (audio reads / DB writes / multi-GB SHA-256).
    "run_t0_gate",
    "import_gold_segments",
    "create_gold_from_file",
    "import_verified_segments_as_gold",
    "import_model_checkpoint",
    # Per-segment acoustic/OOD scoring loops + IRT consensus refinery (decode + ONNX per segment).
    "compute_acoustic_scores",
    "compute_signal_anomaly_scores",
    # DB backup/restore — the heavy file-copy + integrity-check runs in run_blocking.
    "db_backup",
    "db_restore",
    "restore_db_from_snapshot",
    # Media cache: multi-GB fs::copy moved off the main thread (async fn; see the ponytail note).
    "register_media_asset",
    # WSL status probes: shell out to `wsl` (probe_wsl_7b_server TCP probe / `wsl --status`, ~3–10s) —
    # moved to run_blocking so polling the engine/agentic status pills can't freeze the UI.
    "get_champion_engine_status",
    "check_agentic_readiness",
    # Audio-duration watchdog: probe thread + 30s recv_timeout — the whole watchdog now runs on the
    # blocking pool so the recv no longer blocks the UI thread (bound preserved).
    "get_audio_duration",
    # Model downloads: the retained support-asset download-all command is a multi-hundred-MB blocking
    # HTTP fetch, so it remains off the main thread. The old single-model ASR IPC is retired.
    "models_download_all",
    # T2 Gemini audio judge: eager consent/key checks + a brief-locked DB gather, then the N-sample
    # cloud round-trip runs on run_blocking; the verdict write re-locks briefly after the await.
    "run_t2_for_segment",
    # Full jury chain (T0→T1→T2): runs on run_blocking against an owned JuryDbSource (dedicated
    # connection, shared-handle fallback) so neither the UI thread nor the global db Mutex is held
    # across the cloud round-trips.
    "run_jury_pipeline",
    # DPO preference-pair upload: the ~120s blocking outbound HTTP POST moved to run_blocking on a
    # separate WAL connection; the cloud-LLM consent gate stays EAGER (never offload before opt-in).
    "run_dpo_update",
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
    "search_segments",
    "get_dataset_stats",
    "get_intelligence_report",
    "get_audio_health",
    "relink_audio",
    "db_vacuum",
    "merge_dataset_json",
    "get_active_learning_queue",
]

# Query-only segment commands migrated behind the bounded SegmentQueryStore. They still run their
# work on the blocking pool, but must no longer regain the raw serialized-writer handle.
QUERY_STORE_COMMANDS = {
    "get_segments",
    "get_segments_suspect_first",
    "search_segments",
    "get_audio_health",
    "get_active_learning_queue",
}

# Durable export commands whose blocking body is owned by the Tauri-free JobStore. They must not
# regain either the raw writer handle or direct job-lifecycle authority at the IPC boundary.
JOB_STORE_COMMANDS = {
    "export_dataset",
    "export_transcript",
    "export_huggingface_dataset",
    "export_dataset_bundle",
    "export_audio",
    "export_gold_eval_set",
    "export_finetune_pack",
}


def source() -> str:
    """The whole command surface: commands.rs + every extracted slice under src/commands/."""
    if not COMMANDS_RS.is_file():
        raise AssertionError(f"{COMMANDS_RS} is missing — the command surface cannot be checked")
    parts = [COMMANDS_RS.read_text(encoding="utf-8")]
    if COMMANDS_DIR.is_dir():
        parts += [path.read_text(encoding="utf-8") for path in sorted(COMMANDS_DIR.rglob("*.rs"))]
    text = "\n".join(parts)
    # Fail loudly rather than pass vacuously if the surface ever reads empty.
    if "#[tauri::command]" not in text:
        raise AssertionError("no #[tauri::command] found in the command surface — this gate would pass vacuously")
    return text


def command_body(src: str, name: str) -> str:
    start = src.index(f"pub async fn {name}(")
    end = src.find("\n#[tauri::command]", start + len(name))
    return src[start:] if end < 0 else src[start:end]


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
    # Write-capable commands clone the restore-gated Arc handle and lock INSIDE the task. Migrated
    # query commands clone SegmentQueryStore instead. Neither may carry a lock_db() guard across await.
    src = source()
    for name in RUN_BLOCKING_COMMANDS:
        body = command_body(src, name)
        if "state.lock_db()" in body:
            raise AssertionError(f"`{name}` must not carry state.lock_db() into a blocking task")
        if name in QUERY_STORE_COMMANDS:
            if "state.segment_queries()" not in body:
                raise AssertionError(f"`{name}` must obtain its bounded SegmentQueryStore before the blocking task")
            if "state.db_arc()" in body:
                raise AssertionError(f"`{name}` query path regained the raw serialized-writer handle")
        elif name in JOB_STORE_COMMANDS:
            if "state.job_store()" not in body:
                raise AssertionError(f"`{name}` must obtain its tracked JobStore before the blocking task")
            if "state.db_arc()" in body or ".run_tracked(" in body:
                raise AssertionError(f"`{name}` regained raw database or job-lifecycle authority")
        elif "state.db_arc()" not in body:
            raise AssertionError(f"`{name}` must obtain the DB via state.db_arc() for the blocking task")


def main() -> None:
    test_listed_slow_commands_are_async()
    test_off_main_thread_helper_exists_and_is_used()
    test_migrated_exports_do_not_hold_lock_db_across_the_await()
    print("command main-thread policy regression passed")


if __name__ == "__main__":
    main()
