"""UI-thread blocking audit — Week-1 "measure first" worklist + a shrinking-ratchet gate.

Tauri v2 runs a SYNC `#[tauri::command] fn` on the main/UI thread; an `async fn` command runs on the
tokio pool. A sync command that does heavy work — a cloud round-trip, a multi-hundred-MB model
download, a WSL/subprocess spawn, an audio decode/hash, an unbounded DB scan — freezes the window for
the whole operation UNLESS its body offloads that work AND returns before it finishes.

Two traps make "sync == freezes / async == safe" wrong for this repo, so this audit is code-verified,
not marker-guessed:
  1. Several sync commands (the `batch_*` family, `import_audio_file`, `resume_interrupted_import`,
     `run_wsl_refinement`) `std::thread::spawn` the heavy body and return immediately — they do NOT
     freeze the UI. These are tracked in OFFLOADED_HIGH so a future edit can't silently drop the spawn.
  2. Some commands LOOK offloaded or trivial but still block the caller: `get_audio_duration` spawns a
     probe thread then blocks on `rx.recv_timeout(30s)`; the three WSL "status getters"
     (`get_champion_engine_status`, `check_external_provider`, `check_agentic_readiness`) delegate into
     a helper that shells out to `wsl` and blocks 5–10s. Static marker scanning cannot see either
     pattern (the block is behind a delegate / a recv), so the freezer set below is CODE-VERIFIED by
     hand (traced 2026-07-16, cross-checked by a reader agent) and pinned against the source.

FREEZERS is the honest Week-1 migration worklist (item 2 in docs/MONTH_LOOP.md). As each command moves
to `pub async fn` + `run_blocking`, delete it here and add it to test_command_main_thread_policy.py's
ASYNC_SLOW_COMMANDS ratchet — this gate FAILS the moment a listed freezer becomes async, forcing that
bookkeeping so the two ratchets stay in sync and the worklist honestly shrinks.

Real wall-clock per-command timings are NOT produced here — they are owner-gated: they need a real run
(real audio / a live model download / a cloud judge call) on the owner's machine. The TRACER in
`src-tauri/src/telemetry/mod.rs` records real span durations during use (surfaced by `get_recent_spans`
/ `get_tracing_stats`); this script is the static prioritiser that says WHICH commands to time and
migrate first. Full ranked severity + heaviest-op traces live in docs/UI_THREAD_BLOCKING_AUDIT.md.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"

# Sync `#[tauri::command]`s that do heavy work inline on the main thread and freeze the UI. Traced by
# hand into the helper they delegate to. class ∈ {cloud-net, subprocess, file-io}. When one is migrated
# to async, REMOVE it here and add it to ASYNC_SLOW_COMMANDS in test_command_main_thread_policy.py.
FREEZERS: dict[str, tuple[str, str]] = {
    # Cloud round-trips run synchronously on the UI thread (they correctly drop the DB lock first, but
    # the network wait itself still blocks the main thread). All consent/key-gated.
    "run_jury_pipeline": ("cloud-net", "T0/T1/T2 chain; T2 = Gemini audio round-trips (run_jury_pipeline_core_via)"),
    "run_t2_for_segment": ("cloud-net", "N>=3 Gemini audio calls (jury::t2_listener::listen_and_judge_via)"),
    "run_dpo_update": ("cloud-net", "outbound HTTP POST, ~120s cap on a stalled endpoint (jury::learning::run_dpo_update)"),
    "transcribe_audio_with_scribe": ("cloud-net", "audio decode + ElevenLabs Scribe POST (scribe_transcribe_clip)"),
    "add_scribe_votes": ("cloud-net", "per-segment decode/slice + ElevenLabs POST loop (scribe_api::transcribe_wav_bytes)"),
    "models_download": ("cloud-net", "synchronous multi-hundred-MB HTTP download (models::ModelManager::download_model)"),
    "models_download_all": ("cloud-net", "synchronous loop of download_model over all missing models"),
    # Subprocess spawns / WSL probes that block the caller.
    # DEAD (no caller in src/ or Rust; registered only in the invoke_handler) — left sync on purpose,
    # flagged for deletion rather than migration; ponytail: don't invest in code that should be deleted.
    "check_external_provider": ("subprocess", "DEAD (unused) — shells out to `wsl --status` up to 10s (external_provider_status); delete candidate, not a migrate target"),
    "start_champion_engine": ("subprocess", "powershell spawn — detached, so real freeze is just process-creation latency"),
    # Spawns a probe thread but then BLOCKS on recv_timeout(30s) — looks offloaded, is not.
    "get_audio_duration": ("file-io", "spawns a decode probe thread, then blocks on rx.recv_timeout(30s) (audio::get_duration_ms)"),
    # MIGRATED 2026-07-16 (commit after 21ce99f): search_segments — unbounded FTS5 MATCH — moved to
    # `pub async fn` + run_blocking (mirrors get_segments); now in test_command_main_thread_policy.py's
    # ASYNC_SLOW_COMMANDS ratchet. Left here as a breadcrumb, not a live entry.
}

# Sync commands that DO offload (spawn a worker thread and return immediately) — safe today, but the
# gate pins the spawn so an edit can't quietly turn them back into UI-thread blockers.
OFFLOADED_HIGH = [
    "import_audio_file",
    "resume_interrupted_import",
    "batch_transcribe",
    "batch_verify",
    "batch_assign_speaker",
    "batch_normalize",
    "run_wsl_refinement",
]
SPAWN_MARKERS = ["thread::spawn", "Builder::new()", "spawn_blocking", "async_runtime::spawn", "parallel_batch"]


def source() -> str:
    return COMMANDS_RS.read_text(encoding="utf-8")


def _body(src: str, name: str) -> str:
    """Brace-matched body of `pub fn NAME` / `pub async fn NAME`."""
    for sig in (f"pub fn {name}(", f"pub async fn {name}("):
        start = src.find(sig)
        if start == -1:
            continue
        open_brace = src.find("{", start)
        depth, i = 0, open_brace
        while i < len(src):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    return src[open_brace : i + 1]
            i += 1
    return ""


def test_freezers_are_still_sync() -> None:
    src = source()
    for name in FREEZERS:
        if f"pub async fn {name}(" in src:
            raise AssertionError(
                f"`{name}` is now `pub async fn` — migrated off the main thread. Remove it from FREEZERS "
                "and add it to ASYNC_SLOW_COMMANDS in test_command_main_thread_policy.py (the worklist shrank)."
            )
        if f"pub fn {name}(" not in src:
            raise AssertionError(f"freezer `{name}` not found in commands.rs — was it renamed? Update the audit.")


def test_offloaded_high_still_spawn() -> None:
    src = source()
    for name in OFFLOADED_HIGH:
        if f"pub async fn {name}(" in src:
            continue  # migrated to true async — also fine
        if f"pub fn {name}(" not in src:
            raise AssertionError(f"`{name}` not found — update OFFLOADED_HIGH.")
        body = _body(src, name)
        if not any(m in body for m in SPAWN_MARKERS):
            raise AssertionError(
                f"`{name}` is a sync command that no longer spawns a worker thread — it now runs its heavy "
                "body inline on the UI thread and freezes the window. Restore the spawn or make it async."
            )


def main() -> None:
    src = source()
    total_cmds = src.count("#[tauri::command]")
    async_cmds = src.count("pub async fn ")
    print(f"#[tauri::command] total: {total_cmds}   async (off main thread): {async_cmds}")
    print(f"sync + offloaded (spawn-and-return, safe): {len(OFFLOADED_HIGH)}  {OFFLOADED_HIGH}")
    print(f"UI-freeze worklist — sync + heavy + blocks the main thread ({len(FREEZERS)}), migrate first:")
    order = {"cloud-net": 0, "subprocess": 1, "file-io": 2, "db-scan": 3}
    for name, (cls, note) in sorted(FREEZERS.items(), key=lambda kv: (order.get(kv[1][0], 9), kv[0])):
        print(f"    [{cls:10}] {name} — {note}")
    test_freezers_are_still_sync()
    test_offloaded_high_still_spawn()
    print("ui-thread blocking audit passed")


if __name__ == "__main__":
    main()
