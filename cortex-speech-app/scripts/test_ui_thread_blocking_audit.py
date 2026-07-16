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
     probe thread then blocks on `rx.recv_timeout(30s)`; the WSL "status getters"
     (`get_champion_engine_status`, `check_agentic_readiness`; the dead `check_external_provider` was
     deleted) delegate into a helper that shells out to `wsl` and blocks 5–10s. Static marker scanning
     cannot see either
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
    # MIGRATED 2026-07-16: run_jury_pipeline — the whole T0→T1→T2 chain moved to run_blocking against
    # an owned JuryDbSource (with_jury_db extracted); in the ASYNC ratchet.
    # MIGRATED 2026-07-16: run_t2_for_segment — the N-sample Gemini cloud call moved to run_blocking
    # (consent/key gates + brief DB gather stay eager); in the ASYNC ratchet.
    # MIGRATED 2026-07-16: run_dpo_update — the ~120s blocking outbound HTTP POST moved to
    # `pub async fn` + run_blocking on a separate WAL connection (cloud-LLM consent gate stays eager);
    # now in test_command_main_thread_policy.py's ASYNC_SLOW_COMMANDS ratchet. With this, the only
    # remaining freezer is start_champion_engine (a detached powershell spawn whose freeze is just
    # process-creation latency).
    # MIGRATED 2026-07-16: transcribe_audio_with_scribe — blocking Scribe POST moved to run_blocking
    # (consent/key/DB gates stay eager); in the ASYNC ratchet.
    # MIGRATED 2026-07-16: add_scribe_votes — the decode+POST loop moved to run_blocking (consent/key
    # gates + gather stay eager; per-insert brief db_arc lock); in the ASYNC ratchet. With this, every
    # UI-WIRED freezer from the original 13 is off the main thread — the 3 below are unwired/dead/MED.
    # MIGRATED 2026-07-16: models_download + models_download_all — blocking HTTP download moved to
    # run_blocking; in the ASYNC ratchet.
    # Subprocess spawns / WSL probes that block the caller.
    # (check_external_provider was DELETED 2026-07-16 — it was dead code, verified no caller; the
    #  live WSL-status path lives in check_agentic_readiness, already migrated.)
    "start_champion_engine": ("subprocess", "powershell spawn — detached, so real freeze is just process-creation latency"),
    # MIGRATED 2026-07-16: get_audio_duration — the probe-thread + 30s recv_timeout watchdog now runs
    # inside run_blocking (bound preserved, off the UI thread); in the ASYNC ratchet.
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
