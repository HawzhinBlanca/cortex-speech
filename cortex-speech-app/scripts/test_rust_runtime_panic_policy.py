from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
# Week-4 decomposes commands.rs into slices under src/commands/. Every command-CONTENT check here
# must follow a command wherever it moves, or extracting it to a slice would make the check pass
# VACUOUSLY (a forbidden pattern could escape; a required pattern would read as "missing").
COMMANDS_DIR = REPO_ROOT / "src-tauri" / "src" / "commands"


def command_surface() -> str:
    """commands.rs + every extracted slice under src/commands/, concatenated."""
    parts = [(REPO_ROOT / "src-tauri" / "src" / "commands.rs").read_text(encoding="utf-8")]
    if COMMANDS_DIR.is_dir():
        parts += [path.read_text(encoding="utf-8") for path in sorted(COMMANDS_DIR.rglob("*.rs"))]
    text = "\n".join(parts)
    if "#[tauri::command]" not in text:
        raise AssertionError("no #[tauri::command] in the command surface — this gate would pass vacuously")
    return text


def db_surface() -> str:
    """db.rs PLUS db_tests.rs — the db module's tests were split into db_tests.rs (via #[path]) to keep
    db.rs under the size target, so both the production checks AND the test-name assertions below must
    scan the whole db module, or a moved test would read as "missing" and a forbidden pattern could hide
    in the test file."""
    parts = [(REPO_ROOT / "src-tauri" / "src" / "db.rs").read_text(encoding="utf-8")]
    db_tests = REPO_ROOT / "src-tauri" / "src" / "db_tests.rs"
    if db_tests.is_file():
        parts.append(db_tests.read_text(encoding="utf-8"))
    text = "\n".join(parts)
    if "impl Database" not in text:
        raise AssertionError("db.rs surface missing `impl Database` — this gate would pass vacuously")
    return text


def pipeline_surface() -> str:
    """pipeline.rs PLUS pipeline_tests.rs — the test module was split out via #[path], so both the
    production checks AND the test-name assertions below must scan the whole pipeline module."""
    parts = [(REPO_ROOT / "src-tauri" / "src" / "pipeline.rs").read_text(encoding="utf-8")]
    pt = REPO_ROOT / "src-tauri" / "src" / "pipeline_tests.rs"
    if pt.is_file():
        parts.append(pt.read_text(encoding="utf-8"))
    text = "\n".join(parts)
    if "ProcessingPipeline" not in text:
        raise AssertionError("pipeline.rs surface missing `ProcessingPipeline` — this gate would pass vacuously")
    return text


FORBIDDEN_RUNTIME_PATTERNS = {
    "src-tauri/src/jury/t2_listener.rs": [
        "samples.iter().find(|s| s.transcript.trim() == winning_transcript).unwrap()",
    ],
    "src-tauri/src/quality/conformal.rs": [
        "annotated_transcript.as_ref().unwrap()",
    ],
    "src-tauri/src/quality/signal_anomaly.rs": [
        "session_mutex.lock().map_err",
        "OOD lock:",
    ],
    "src-tauri/src/jury/mod.rs": [
        ".unwrap_or(0)",
        ".unwrap_or_default()",
        "rows.filter_map(|r| r.ok()).collect()",
    ],
    "src-tauri/src/jury/learning.rs": [
        "filter_map(|p| serde_json::to_string(p).ok())",
    ],
    "src-tauri/src/db.rs": [
        "Ok(rows.filter_map(|r| r.ok()).collect())",
    ],
    "src-tauri/src/eval.rs": [
        "Ok(rows.filter_map(|r| r.ok()).collect())",
    ],
    "src-tauri/src/commands.rs": [
        "WSL_CHILD.lock().unwrap()",
        "state.pipeline.lock()",
        "state.settings.lock()",
        "app_state.settings.lock()",
        "state.model_manager.lock()",
        "app_state.model_manager.lock()",
        "state.data_dir.lock()",
        "app_state.data_dir.lock()",
        "state.media_registry.lock()",
        "app_state.media_registry.lock()",
        "state.history.lock()",
        "app_state.history.lock()",
        "state.session.lock()",
        "app_state.session.lock()",
        "state.db.lock()",
        "app_state.db.lock()",
        "get_segments_by_ids(&ids).unwrap_or_default()",
        "get_segments_by_ids(&segment_ids).unwrap_or_default()",
        "update_verified(id, verified).unwrap_or(false)",
        "update_speaker_id(id, spk).unwrap_or(false)",
        "get_hypotheses_for_segment(seg_id).unwrap_or_default()",
        "get_few_shot_examples(db, seg_id, 5).unwrap_or_default()",
        "get_few_shot_examples(&db, &segment_id, 5).unwrap_or_default()",
        "db.get_segment_by_id(id).ok().flatten()",
        "db.insert_segment(&seg).is_ok()",
        "app_state.lock_db().insert_segment(&seg).is_ok()",
        "serde_json::to_string(&evidence).ok()",
        "serde_json::to_string(&verdict.evidence).ok()",
        "if let Ok((sample_rate, pcm)) = audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30))",
        "if let Ok((_sr, pcm_16k)) = audio::ensure_pcm_16khz(sample_rate, pcm)",
        "if let Ok(score) = aligner.score_consistency(&pcm_16k, audio::TARGET_SAMPLE_RATE, &text)",
        "let _ = run_jury_pipeline_core",
        "state.cancel_token.lock().map_err",
        "state.cancel_token.lock().ok",
        "state.import_state.lock().map_err",
        "state.import_state.lock()",
        "state.batch_state.lock().map_err",
        "state.batch_state.lock()",
        "app_state.batch_state.lock()",
        "use crate::BatchState;",
    ],
    "src-tauri/src/throttle.rs": [
        'expect("just inserted")',
        "rate limiter poisoned",
    ],
    "src-tauri/src/fingerprint.rs": [
        "fingerprint lock poisoned",
        "self.known.lock().map",
        "if let Ok(map) = self.known.lock()",
        "if let Ok(mut map) = self.known.lock()",
    ],
    "src-tauri/src/cache.rs": [
        "self.store.lock().ok",
        "self.store.lock().map",
        "if let Ok(mut store) = self.store.lock()",
    ],
    "src-tauri/src/perf/mod.rs": [
        "NonZeroUsize::new(capacity.max(1)).unwrap()",
        "if let Ok(mut cache) = self.cache.lock()",
        "self.cache.lock().map",
    ],
    "src-tauri/src/history/mod.rs": [
        "Lock error",
        "if let Ok(mut stack) = self.undo_stack.lock()",
        "if let Ok(mut stack) = self.redo_stack.lock()",
        "self.undo_stack.lock().map",
        "self.redo_stack.lock().map",
        "self.undo_stack.lock().ok",
        "self.redo_stack.lock().ok",
    ],
    "src-tauri/src/inference.rs": [
        "if let Ok(mut v) = INFERENCE_METRICS.vad_latencies.lock()",
        "if let Ok(mut v) = INFERENCE_METRICS.asr_latencies.lock()",
        "if let Ok(mut v) = INFERENCE_METRICS.align_latencies.lock()",
        "vad_latencies.lock().map",
        "asr_latencies.lock().map",
        "unwrap_or_default()",
        "unwrap_or_else(|e| e.into_inner())",
    ],
    "src-tauri/src/lib.rs": [
        "INFERENCE_METRICS.model_load_time_ms.lock()",
        "self.settings.lock().map_err",
        "self.settings.lock().ok",
        "self.model_manager.lock().map_err",
        "self.model_manager.lock().ok",
        "self.data_dir.lock().map_err",
        "self.data_dir.lock().ok",
        "self.media_registry.lock().map_err",
        "self.media_registry.lock().ok",
        "self.history.lock().map_err",
        "self.history.lock().ok",
        "self.session.lock().map_err",
        "self.session.lock().ok",
        "self.session.try_lock()",
        "self.db.try_lock()",
        "self.db.lock().map_err",
        "self.cancel_token.lock().map_err",
        "self.cancel_token.lock().ok",
        "self.import_state.lock().map_err",
        "self.import_state.lock().ok",
        "self.batch_state.lock().map_err",
        "self.batch_state.lock().ok",
        "let _ = self.lock_session().save(&db)",
        "let _ = self.lock_session().auto_save(&db)",
    ],
    "src-tauri/src/integration_runner.rs": [
        "state.pipeline.lock()",
        "state.settings.lock()",
        "state.db.lock()",
    ],
    "src-tauri/src/audio.rs": [
        "NonZeroUsize::new(10).unwrap()",
        "PCM_CACHE.lock().unwrap_or_else(|e| e.into_inner())",
        "if let Ok(mut cache) = PCM_CACHE.lock()",
        "VAD_CACHE.lock().map_err",
        "if let Ok(mut guard) = VAD_CACHE.lock()",
    ],
    "src-tauri/src/health.rs": [
        "if let Ok(mut sys) = SYS.lock()",
        "SYS.lock().ok",
        "SYS.lock().map",
    ],
    "src-tauri/src/asr.rs": [
        "self.inner.lock().unwrap_or_else(|e| e.into_inner())",
        "self.inner.lock().map",
        "self.inner.lock().ok",
    ],
    "src-tauri/src/pipeline.rs": [
        # The WSL-import rollback sites log "rolling back N segment(s)" and then delete — a swallowed
        # delete failure means the log promised a rollback that never happened, leaving placeholder rows
        # that duplicate on re-import. Every rollback delete must report its own failure.
        "let _ = db.delete_segments_batch(",
        "if let Ok(mut status) = self.import_status.lock()",
        "self.import_status.lock().map",
        "self.import_status.lock().ok",
        "self.import_status.lock().unwrap_or_else(|e| e.into_inner())",
        "self.diarization_service.lock().unwrap_or_else(|e| e.into_inner())",
        "self.denoiser_service.lock().unwrap_or_else(|e| e.into_inner())",
        "acc.lock().unwrap_or_else(|e| e.into_inner())",
        'windows.lock().map_err(|_| AppError::Validation("Poisoned lock".into()))',
        "db.insert_segment(&seg).is_ok()",
        "let _ = crate::commands::run_jury_pipeline_core",
        "let _ = self.populate_hypotheses",
        "let _ = db.insert_hypothesis",
        "asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE).ok()",
    ],
    "src-tauri/src/bin/batch_processor.rs": [
        "asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).unwrap_or_default()",
    ],
    "src-tauri/src/telemetry/mod.rs": [
        "self.spans.lock().unwrap_or_else(|e| e.into_inner())",
        "self.tracer.spans.lock().unwrap_or_else(|e| e.into_inner())",
        "self.spans.lock().map",
        "self.spans.lock().ok",
        "self.tracer.spans.lock().map",
        "self.tracer.spans.lock().ok",
    ],
}


def test_known_runtime_panic_patterns_do_not_return() -> None:
    offenders: list[str] = []
    for relative_path, patterns in FORBIDDEN_RUNTIME_PATTERNS.items():
        # commands.rs forbidden patterns are checked across the whole command surface (incl. slices)
        # so a bad pattern cannot slip past by being extracted into src/commands/.
        if relative_path == "src-tauri/src/commands.rs":
            text = command_surface()
        elif relative_path == "src-tauri/src/db.rs":
            text = db_surface()
        elif relative_path == "src-tauri/src/pipeline.rs":
            text = pipeline_surface()
        else:
            text = (REPO_ROOT / relative_path).read_text(encoding="utf-8")
        for pattern in patterns:
            if pattern in text:
                offenders.append(f"{relative_path}: {pattern}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(f"Known runtime panic-prone patterns returned:\n{formatted}")


def test_wsl_refinement_batch_is_panic_safe_and_cancellable() -> None:
    commands = command_surface()
    # The batch 7B refinement no longer spawns the configured script once with batch flags the
    # per-segment warm client cannot parse; it drives the shared per-segment helper in a loop. These
    # invariants keep that loop process-safe, cancellable, and non-destructive.
    required = [
        # Single-run guard: a second batch cannot run concurrently over the same segments.
        "static WSL_REFINE_RUNNING: std::sync::atomic::AtomicBool",
        "WSL_REFINE_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst)",
        # Cancellation flag, set by the cancel command and polled between segments + in-flight.
        "static WSL_REFINE_CANCEL: std::sync::atomic::AtomicBool",
        "WSL_REFINE_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst)",
        # The running flag clears even if the worker thread panics mid-batch (RAII guard on Drop),
        # and the guard ALSO clears CANCEL at run end (so no start-of-run reset clobbers a racing cancel).
        "impl Drop for WslRefineRunningGuard",
        "WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst)",
        "WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst)",
        # Builder::spawn returns Err instead of panicking on OS thread-creation failure, so a failed
        # spawn cannot wedge WSL_REFINE_RUNNING true (the RAII guard lives inside the closure).
        'std::thread::Builder::new().name("wsl-7b-batch".into()).spawn(',
        # A panic in the loop still emits a terminal wsl-status so the panel never wedges at "running".
        "std::panic::catch_unwind(std::panic::AssertUnwindSafe(",
        # Drive the shared per-segment warm client (not a one-shot batch spawn), passing the cancel
        # flag so a long clip is interrupted promptly instead of blocking the whole batch. (Checked as
        # two robust tokens rather than one exact call string, which rustfmt wraps across lines once the
        # client also receives db_path — the safety invariant is: the client is driven AND the cancel
        # flag is passed to it.)
        "run_wsl_segment_transcript_with_script(",
        "Some(&WSL_REFINE_CANCEL)",
        # Writes go through the human-decision-safe update so a batch never clobbers reviewed text,
        # and the 7B provenance is persisted instead of falling back to unknown/heuristic.
        "db.update_asr_transcript_if_unreviewed(",
        'Some("external_provider")',
        'Some("omniasr-wsl-7b")',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs WSL batch refinement lost a safety invariant:\n{formatted}")


def test_wsl_refinement_lifecycle_failures_are_reported() -> None:
    commands = command_surface()
    forbidden = [
        "child.wait().ok()",
        "let _ = child.kill();",
    ]
    present = [pattern for pattern in forbidden if pattern in commands]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"commands.rs silently discards WSL refinement lifecycle failures:\n{formatted}")

    required = [
        # The detached batch worker reports a terminal failure as an event instead of swallowing it.
        'emit_or_log(&app, "wsl-log", format!("[ERROR] {}", wsl_log_preview(&message)));',
        # The terminal status carries transcribed AND failed so the UI is honest about partial failure
        # (a run with failures is never a plain green success).
        'serde_json::json!({ "status": status, "transcribed": transcribed, "failed": failed, "exit_code": exit_code })',
        # Per-segment failures and human-reviewed skips are surfaced, not hidden.
        "failed += 1;",
        "skipped (human-reviewed; transcript not overwritten)",
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs must keep observable WSL refinement lifecycle handling:\n{formatted}")


def test_app_entrypoint_reports_fatal_errors_without_panicking() -> None:
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    forbidden = [
        'panic!("Failed to create app data directory',
        'panic!("Failed to open database',
        'panic!("Failed to initialize database schema',
        'panic!("Tauri application runtime error',
    ]
    offenders = [pattern for pattern in forbidden if pattern in lib]
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(f"App entrypoint startup panics returned:\n{formatted}")
    if "fn fatal_app_error(message: String) -> !" not in lib:
        raise AssertionError("lib.rs must keep app startup/runtime fatal errors behind fatal_app_error()")
    if "CORTEX_STARTUP_FAIL" not in lib:
        raise AssertionError("fatal_app_error() must emit a stable CORTEX_STARTUP_FAIL marker")
    exit_count = lib.count("std::process::exit(1)")
    if exit_count != 1:
        raise AssertionError(f"lib.rs must only exit from fatal_app_error(), found {exit_count} exits")


def test_app_state_cancel_token_recovers_poisoned_lock() -> None:
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    required_locks = [
        "fn lock_import_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>>",
        "fn lock_batch_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>>",
    ]
    missing_locks = [lock for lock in required_locks if lock not in lib]
    if missing_locks:
        formatted = "\n".join(f"- {entry}" for entry in missing_locks)
        raise AssertionError(f"AppState must centralize cancellation token locking per operation kind:\n{formatted}")
    required_warnings = [
        "Recovering poisoned import cancellation token lock",
        "Recovering poisoned batch cancellation token lock",
    ]
    missing_warnings = [warning for warning in required_warnings if warning not in lib]
    if missing_warnings:
        formatted = "\n".join(f"- {entry}" for entry in missing_warnings)
        raise AssertionError(f"AppState must warn when recovering poisoned cancellation token locks:\n{formatted}")
    if "poisoned.into_inner()" not in lib:
        raise AssertionError("AppState cancellation token locking must recover with poisoned.into_inner()")
    import_lock_count = lib.count("self.import_cancel_token.lock()")
    batch_lock_count = lib.count("self.batch_cancel_token.lock()")
    if import_lock_count != 1:
        raise AssertionError(f"self.import_cancel_token.lock() must only appear inside lock_import_cancel_token(), found {import_lock_count}")
    if batch_lock_count != 1:
        raise AssertionError(f"self.batch_cancel_token.lock() must only appear inside lock_batch_cancel_token(), found {batch_lock_count}")
    if "app_state_cancel_token_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned cancellation token recovery")
    if (
        "pub fn is_cancelled(&self) -> bool" not in lib
        or "self.lock_import_cancel_token()" not in lib
        or "self.lock_batch_cancel_token()" not in lib
    ):
        raise AssertionError("AppState::is_cancelled() must read through recovered per-operation token locks")
    if "pub fn start_cancel_token(&self) -> CancellationToken" not in lib:
        raise AssertionError("AppState must expose start_cancel_token() for command handlers")
    if "pub fn ensure_cancel_token(&self) -> Result<CancellationToken, String>" not in lib:
        raise AssertionError("AppState must expose ensure_cancel_token() for batch command handlers")
    if "pub fn cancel_current_operation(&self) -> bool" not in lib:
        raise AssertionError("AppState must expose cancel_current_operation() for command handlers")
    if "app_state_start_and_cancel_recover_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned start/cancel recovery")


def test_commands_use_recovered_app_state_cancellation_api() -> None:
    commands = command_surface()
    if "state.start_cancel_token()" not in commands:
        raise AssertionError("commands.rs must start operations through AppState::start_cancel_token()")
    if "state.cancel_current_operation()" not in commands:
        raise AssertionError("commands.rs must cancel operations through AppState::cancel_current_operation()")
    if "state.cancel_token.lock()" in commands:
        raise AssertionError("commands.rs must not lock cancel_token directly")


def test_app_state_import_state_recovers_poisoned_lock() -> None:
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    if "fn lock_import_state(&self) -> MutexGuard<'_, ImportState>" not in lib:
        raise AssertionError("AppState must centralize import state locking behind lock_import_state()")
    if "Recovering poisoned import state lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned import state lock")
    if "pub fn try_start_import(&self) -> Result<(), String>" not in lib:
        raise AssertionError("AppState must expose try_start_import() for import command handlers")
    if "pub fn finish_import(&self)" not in lib:
        raise AssertionError("AppState must expose finish_import() for import command cleanup")
    direct_lock_count = lib.count("self.import_state.lock()")
    if direct_lock_count != 1:
        raise AssertionError(f"self.import_state.lock() must only appear inside lock_import_state(), found {direct_lock_count}")
    if "app_state_import_state_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned import-state recovery")


def test_commands_use_recovered_app_state_import_api() -> None:
    commands = command_surface()
    if "state.try_start_import()?" not in commands:
        raise AssertionError("commands.rs must start imports through AppState::try_start_import()")
    if "finish_import()" not in commands:
        raise AssertionError("commands.rs must finish imports through AppState::finish_import()")
    if "import_state.lock()" in commands:
        raise AssertionError("commands.rs must not lock import_state directly")
    if "use crate::ImportState;" in commands:
        raise AssertionError("commands.rs must not import ImportState for direct command-state checks")


def test_app_state_batch_state_recovers_poisoned_lock() -> None:
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    if "fn lock_batch_state(&self) -> MutexGuard<'_, BatchState>" not in lib:
        raise AssertionError("AppState must centralize batch state locking behind lock_batch_state()")
    if "Recovering poisoned batch state lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned batch state lock")
    if "pub fn try_start_batch(&self) -> Result<(), String>" not in lib:
        raise AssertionError("AppState must expose try_start_batch() for batch command handlers")
    if "pub fn finish_batch(&self)" not in lib:
        raise AssertionError("AppState must expose finish_batch() for batch command cleanup")
    direct_lock_count = lib.count("self.batch_state.lock()")
    if direct_lock_count != 1:
        raise AssertionError(f"self.batch_state.lock() must only appear inside lock_batch_state(), found {direct_lock_count}")
    if "app_state_batch_state_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned batch-state recovery")


def test_commands_use_recovered_app_state_batch_api() -> None:
    commands = command_surface()
    if "state.try_start_batch()?" not in commands:
        raise AssertionError("commands.rs must start batches through AppState::try_start_batch()")
    if "finish_batch()" not in commands:
        raise AssertionError("commands.rs must finish batches through AppState::finish_batch()")
    if "batch_state.lock()" in commands:
        raise AssertionError("commands.rs must not lock batch_state directly")
    if "use crate::BatchState;" in commands:
        raise AssertionError("commands.rs must not import BatchState for direct command-state checks")


def test_app_state_pipeline_settings_update_recovers_poisoned_lock() -> None:
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    pipeline = pipeline_surface()
    commands = command_surface()
    if "pub(crate) fn lock_pipeline(&self) -> MutexGuard<'_, ProcessingPipeline>" not in lib:
        raise AssertionError("AppState must centralize processing pipeline locking behind lock_pipeline()")
    if "Recovering poisoned processing pipeline lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned processing pipeline lock")
    if "pub(crate) fn lock_settings(&self) -> MutexGuard<'_, AppSettings>" not in lib:
        raise AssertionError("AppState must centralize settings locking behind lock_settings()")
    if "Recovering poisoned settings lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned settings lock")
    if "app_state_settings_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned settings recovery")
    if "pub(crate) fn lock_model_manager(&self) -> MutexGuard<'_, ModelManager>" not in lib:
        raise AssertionError("AppState must centralize model manager locking behind lock_model_manager()")
    if "Recovering poisoned model manager lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned model manager lock")
    if "app_state_model_manager_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned model manager recovery")
    if "pub(crate) fn lock_data_dir(&self) -> MutexGuard<'_, Option<PathBuf>>" not in lib:
        raise AssertionError("AppState must centralize data directory locking behind lock_data_dir()")
    if "Recovering poisoned data directory lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned data directory lock")
    if "app_state_data_dir_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned data directory recovery")
    if "pub(crate) fn lock_media_registry(&self) -> MutexGuard<'_, MediaRegistry>" not in lib:
        raise AssertionError("AppState must centralize media registry locking behind lock_media_registry()")
    if "Recovering poisoned media registry lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned media registry lock")
    if "app_state_media_registry_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned media registry recovery")
    if "pub(crate) fn lock_history(&self) -> MutexGuard<'_, HistoryManager>" not in lib:
        raise AssertionError("AppState must centralize history locking behind lock_history()")
    if "Recovering poisoned history lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned history lock")
    if "app_state_history_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned history recovery")
    if "pub(crate) fn lock_session(&self) -> MutexGuard<'_, SessionManager>" not in lib:
        raise AssertionError("AppState must centralize session locking behind lock_session()")
    if "Recovering poisoned session lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned session lock")
    if "app_state_session_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned session recovery")
    if 'tracing::error!("Session save failed: {error}");' not in lib:
        raise AssertionError("AppState::session_save() must log session save failures")
    if 'tracing::error!("Session autosave failed: {error}");' not in lib:
        raise AssertionError("AppState::session_auto_save() must log session autosave failures")
    if "pub(crate) fn lock_db(&self) -> MutexGuard<'_, Database>" not in lib:
        raise AssertionError("AppState must centralize database locking behind lock_db()")
    if "Recovering poisoned database lock" not in lib:
        raise AssertionError("AppState must warn when recovering a poisoned database lock")
    if "app_state_db_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned database recovery")
    if "pub fn update_pipeline_settings(&self, settings: AppSettings)" not in lib:
        raise AssertionError("AppState must expose update_pipeline_settings() for command handlers")
    if "app_state_pipeline_settings_update_recovers_poisoned_lock" not in lib:
        raise AssertionError("lib.rs must keep a unit test for poisoned pipeline settings updates")
    if "pub fn settings_snapshot(&self) -> AppSettings" not in pipeline:
        raise AssertionError("ProcessingPipeline must expose settings_snapshot() for regression verification")
    # Accept either a by-value move or a `.clone()` — update_settings legitimately clones because it
    # still needs `settings` for the disk save AFTER applying to the pipeline (apply-before-save, so the
    # session reflects the change even if the save fails). The gate's intent is that the refresh goes
    # through AppState::update_pipeline_settings, which both forms satisfy.
    if (
        "state.update_pipeline_settings(settings);" not in commands
        and "state.update_pipeline_settings(settings.clone());" not in commands
    ):
        raise AssertionError("commands.rs update_settings must refresh the live pipeline through AppState")
    if "state.pipeline.lock()" in commands:
        raise AssertionError("commands.rs must not lock the processing pipeline directly")
    if "state.settings.lock()" in commands or "app_state.settings.lock()" in commands:
        raise AssertionError("commands.rs must not lock settings directly")
    if "state.model_manager.lock()" in commands or "app_state.model_manager.lock()" in commands:
        raise AssertionError("commands.rs must not lock the model manager directly")
    if "state.data_dir.lock()" in commands or "app_state.data_dir.lock()" in commands:
        raise AssertionError("commands.rs must not lock the data directory directly")
    if "state.media_registry.lock()" in commands or "app_state.media_registry.lock()" in commands:
        raise AssertionError("commands.rs must not lock the media registry directly")
    if "state.history.lock()" in commands or "app_state.history.lock()" in commands:
        raise AssertionError("commands.rs must not lock history directly")
    if "state.session.lock()" in commands or "app_state.session.lock()" in commands:
        raise AssertionError("commands.rs must not lock session directly")


def test_session_cleanup_reports_failures() -> None:
    session = (REPO_ROOT / "src-tauri/src/session/mod.rs").read_text(encoding="utf-8")
    forbidden = [
        "std::fs::create_dir_all(&save_dir).ok();",
        "let _ = std::fs::remove_file(self.save_path());",
    ]
    present = [pattern for pattern in forbidden if pattern in session]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"session/mod.rs silently discards session lifecycle cleanup failures:\n{formatted}")

    required = [
        'tracing::warn!("Failed to create session directory {}: {error}", save_dir.display());',
        "match std::fs::remove_file(&path)",
        "Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}",
        'tracing::warn!("Failed to remove session file {}: {error}", path.display()),',
    ]
    missing = [pattern for pattern in required if pattern not in session]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"session/mod.rs must keep observable session cleanup handling:\n{formatted}")


def test_instance_lock_cleanup_reports_failures() -> None:
    flock = (REPO_ROOT / "src-tauri/src/flock.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = std::fs::remove_file(&lock_path);",
        "let _ = std::fs::remove_file(&self.path);",
    ]
    present = [pattern for pattern in forbidden if pattern in flock]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"flock.rs silently discards instance lock cleanup failures:\n{formatted}")

    required = [
        "fn remove_lock_file(path: &Path, context: &str)",
        'remove_lock_file(&lock_path, "stale Windows instance lock");',
        'remove_lock_file(&lock_path, "failed Unix instance lock acquisition");',
        'remove_lock_file(&PathBuf::from(&self.path), "released instance lock");',
        "Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}",
        'tracing::warn!("Failed to remove {context} file {}: {error}", path.display()),',
    ]
    missing = [pattern for pattern in required if pattern not in flock]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"flock.rs must keep observable instance lock cleanup handling:\n{formatted}")


def test_commands_do_not_silently_default_critical_db_failures() -> None:
    commands = command_surface()
    required = [
        'tracing::error!("Batch transcribe DB prefetch failed: {error}")',
        'let segments = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;',
        'tracing::error!("Batch verify DB update failed for {id}: {error}")',
        'tracing::error!("Batch speaker assignment DB update failed for {id}: {error}")',
        '.get_segments_by_ids(&segment_ids)\n        .map_err(|e| e.to_string())?',
        'let mut hyps = db.get_hypotheses_for_segment(seg_id).map_err(|e| e.to_string())?;',
        'let few_shots = crate::jury::get_few_shot_examples(db, seg_id, 5).map_err(|e| e.to_string())?;',
        'let few_shots = crate::jury::get_few_shot_examples(&db, &segment_id, 5).map_err(|e| e.to_string())?;',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit DB failure handling:\n{formatted}")


def test_commands_batch_transcribe_reports_insert_failures() -> None:
    commands = command_surface()
    # Batch transcribe writes results through a GUARDED targeted update
    # (update_batch_transcription_if_unreviewed) rather than a full insert_segment of the prefetched
    # (now-stale) snapshot, so a concurrent human verify/edit is never clobbered. It must STILL snapshot
    # the pre-transcription row for undo and report DB-write failures explicitly (never swallow them).
    required = [
        "match app_state.lock_db().update_batch_transcription_if_unreviewed(",
        'tracing::error!("Batch transcribe DB update failed for {id}: {error}")',
        "previous_segments.push(pre_transcription_snapshot);",
        "transcribed_ids.push(id.clone());",
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit batch-transcribe write-failure handling:\n{formatted}")


def test_commands_jury_evidence_serialization_is_not_silent() -> None:
    commands = command_surface()
    required = [
        'format!("Failed to serialize T1 evidence for {seg_id}: {e}")',
        'format!("Failed to serialize T2 evidence for {seg_id}: {e}")',
        'format!("Failed to serialize T2 evidence for {segment_id}: {e}")',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit jury evidence serialization errors:\n{formatted}")


def test_jury_background_runs_report_failures() -> None:
    commands = command_surface()
    pipeline = pipeline_surface()
    required_commands = [
        'fn log_jury_pipeline_failure(context: &str, error: &str)',
        'tracing::error!("Jury pipeline failed after {context}: {error}");',
        'log_jury_pipeline_failure("single-file import", &error);',
        'log_jury_pipeline_failure("batch transcription", &error);',
    ]
    missing_commands = [pattern for pattern in required_commands if pattern not in commands]
    if missing_commands:
        formatted = "\n".join(f"- {entry}" for entry in missing_commands)
        raise AssertionError(f"commands.rs is missing explicit background jury failure logging:\n{formatted}")

    required_pipeline = [
        'match crate::commands::run_jury_pipeline_core(&db, &self.settings, imported_ids.clone())',
        'Post-import jury adjudication failed after directory import',
        'callback(PipelineEvent::Error { file: "post-import jury".into(), error: message.clone() });',
        'return Err(AppError::Other(message));',
    ]
    missing_pipeline = [pattern for pattern in required_pipeline if pattern not in pipeline]
    if missing_pipeline:
        formatted = "\n".join(f"- {entry}" for entry in missing_pipeline)
        raise AssertionError(f"pipeline.rs is missing explicit background jury failure logging:\n{formatted}")


def test_pipeline_event_emits_report_failures() -> None:
    commands = command_surface()
    required = [
        "fn emit_or_log<T>(app: &tauri::AppHandle, event: &str, payload: T)",
        'tracing::warn!("Failed to emit {event}: {error}");',
        'emit_or_log(app, "pipeline-started"',
        'emit_or_log(app, "pipeline-phase"',
        'emit_or_log(\n                app,\n                "pipeline-progress"',
        'emit_or_log(app, "pipeline-complete", payload.clone());',
        'emit_or_log(app, "import-complete", payload);',
        'emit_or_log(\n                app,\n                "pipeline-error"',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit pipeline event emit failure logging:\n{formatted}")


def test_command_event_emits_are_not_silently_discarded() -> None:
    commands = command_surface()
    forbidden = [
        "let _ = app.emit(",
        "let _ = app_clone.emit(",
        "let _ = app_stdout.emit(",
        "let _ = app_stderr.emit(",
        "let _ = app_clone\n            .emit(",
    ]
    present = [pattern for pattern in forbidden if pattern in commands]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"commands.rs silently discards Tauri event emit failures:\n{formatted}")

    required_events = [
        '"batch-progress"',
        '"import-complete"',
        '"model-download-progress"',
        '"pipeline-complete"',
        '"pipeline-error"',
        '"wsl-log"',
        '"wsl-status"',
    ]
    missing = [event for event in required_events if f"emit_or_log(&" not in commands or event not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing centralized event emit logging for:\n{formatted}")


def test_commands_audio_duration_probe_send_failures_are_reported() -> None:
    commands = command_surface()
    forbidden = [
        "let _ = tx.send(result);",
    ]
    present = [pattern for pattern in forbidden if pattern in commands]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"commands.rs silently discards audio duration probe send failures:\n{formatted}")

    required = [
        "fn send_audio_duration_probe_result(",
        "send_audio_duration_probe_result(tx, result);",
        'tracing::warn!("Audio duration probe worker could not send result; receiver was dropped or timed out");',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs must keep observable audio duration probe send handling:\n{formatted}")


def test_commands_batch_normalize_reports_prefetch_and_update_failures() -> None:
    commands = command_surface()
    required = [
        'tracing::warn!("Batch normalize segment not found during prefetch: {id}")',
        'tracing::error!("Batch normalize DB prefetch failed for {id}: {error}")',
        'tracing::error!("Batch normalize app state unavailable during prefetch")',
        "let mut failed = prefetch_failed_ids.len() as u32;",
        '"file": id, "status": "failed", "operation": "normalize"',
        'tracing::error!("Batch normalize DB update failed for {id}: {error}")',
        'tracing::warn!("Batch normalize segment disappeared before update: {id}")',
        'tracing::error!("Batch normalize app state unavailable before update for {id}")',
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit batch-normalize failure handling:\n{formatted}")


def test_commands_acoustic_scoring_reports_skipped_segments() -> None:
    commands = command_surface()
    required = [
        '"Skipping acoustic score for {}: audio path not found: {}"',
        'tracing::warn!("Skipping acoustic score for {}: decode failed: {error}", seg.id);',
        '"Skipping acoustic score for {}: 16 kHz conversion failed: {error}"',
        'tracing::warn!("Skipping acoustic score for {}: scoring failed: {error}", seg.id);',
        "let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30))",
        "let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm)",
        "let score = match aligner.score_consistency(&pcm_16k, audio::TARGET_SAMPLE_RATE, &text)",
    ]
    missing = [pattern for pattern in required if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs is missing explicit acoustic scoring skip handling:\n{formatted}")


def test_alignment_json_and_quality_are_written_as_one_atomic_statement() -> None:
    # Word timings and their quality marker must land TOGETHER: quality.rs raises the
    # energy_heuristic_alignment review-risk reason only when the marker is PRESENT, so timings written
    # without their marker read as trustworthy alignment. The old two-statement pair
    # (update_segment_alignment_json then update_alignment_quality) had that window at both call sites —
    # and the background aligner swallowed the second write outright (`let _ =`), silently laundering
    # heuristic timestamps whenever the stamp failed. Both sites must use the combined atomic
    # db.update_segment_alignment(...) and report its failure (this gate's original intent: the stamp
    # outcome is never swallowed).
    commands = command_surface()
    pipeline = pipeline_surface()

    for name, text in (("commands", commands), ("pipeline.rs", pipeline)):
        for stale in ("update_segment_alignment_json(", "update_alignment_quality("):
            if stale in text:
                raise AssertionError(
                    f"{name} writes alignment via the split two-statement pair ({stale}) — timings and "
                    "their quality marker must be one atomic db.update_segment_alignment(...) write"
                )

    required_commands = [
        "db.update_segment_alignment(id, &merged, quality.as_db_str())",
        'map_err(|error| format!("Failed to persist word timings + quality for {id}: {error}"))?;',
    ]
    missing = [pattern for pattern in required_commands if pattern not in commands]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"commands.rs must keep observable atomic alignment persistence:\n{formatted}")

    required_pipeline = [
        "if let Err(error) = db.update_segment_alignment(",
        'tracing::warn!("background alignment: persist failed for {seg_id}: {error}");',
    ]
    missing = [pattern for pattern in required_pipeline if pattern not in pipeline]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"pipeline.rs background aligner must report atomic alignment persist failures:\n{formatted}")


def test_media_cache_cleanup_reports_failures() -> None:
    media = (REPO_ROOT / "src-tauri/src/media.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = std::fs::remove_file(record.cached_path);",
    ]
    present = [pattern for pattern in forbidden if pattern in media]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"media.rs silently discards cached media cleanup failures:\n{formatted}")

    required = [
        "fn remove_cached_media_file(path: &Path, context: &str)",
        'remove_cached_media_file(&record.cached_path, "stale grant");',
        'remove_cached_media_file(&record.cached_path, "expired grant");',
        "Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}",
        'tracing::warn!("Failed to remove {context} cached media file {}: {error}", path.display()),',
    ]
    missing = [pattern for pattern in required if pattern not in media]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"media.rs must keep observable cached media cleanup handling:\n{formatted}")


def test_jury_db_and_export_paths_do_not_silently_drop_errors() -> None:
    jury = (REPO_ROOT / "src-tauri/src/jury/mod.rs").read_text(encoding="utf-8")
    learning = (REPO_ROOT / "src-tauri/src/jury/learning.rs").read_text(encoding="utf-8")
    export = (REPO_ROOT / "src-tauri/src/export.rs").read_text(encoding="utf-8")
    required_jury = [
        "db.record_human_decision(segment_id, decision, corrected_transcript, timestamp_ms)",
        "rows.collect::<Result<Vec<_>, _>>()?",
    ]
    missing_jury = [pattern for pattern in required_jury if pattern not in jury]
    if missing_jury:
        formatted = "\n".join(f"- {entry}" for entry in missing_jury)
        raise AssertionError(f"jury/mod.rs is missing explicit DB error propagation:\n{formatted}")
    if jury.count("rows.collect::<Result<Vec<_>, _>>()?") != 3:
        raise AssertionError("jury/mod.rs must collect all three mapped row iterators with error propagation")

    required_learning = [
        "pairs.iter().map(serde_json::to_string).collect::<Result<Vec<_>, _>>()?.join",
    ]
    missing_learning = [pattern for pattern in required_learning if pattern not in learning]
    if missing_learning:
        formatted = "\n".join(f"- {entry}" for entry in missing_learning)
        raise AssertionError(f"jury/learning.rs is missing explicit JSONL serialization error propagation:\n{formatted}")

    forbidden_export = [
        "let _ = db.update_segment_split(&seg.id, assigned_split);",
    ]
    present_export = [pattern for pattern in forbidden_export if pattern in export]
    if present_export:
        formatted = "\n".join(f"- {entry}" for entry in present_export)
        raise AssertionError(f"export.rs silently discards HuggingFace split persistence failures:\n{formatted}")

    required_export = [
        "db.update_segment_split(id, split)",
        'AppError::Other(format!("Failed to persist split {split} for {id}: {error}"))',
    ]
    missing_export = [pattern for pattern in required_export if pattern not in export]
    if missing_export:
        formatted = "\n".join(f"- {entry}" for entry in missing_export)
        raise AssertionError(f"export.rs must keep observable HuggingFace split persistence handling:\n{formatted}")


def test_database_read_paths_do_not_silently_drop_rows() -> None:
    db = db_surface()
    required = [
        "pub fn get_segments_by_ids(&self, ids: &[String]) -> AppResult<Vec<SpeechSegment>>",
        "pub fn get_escalation_queue(&self, limit: usize) -> AppResult<Vec<SpeechSegment>>",
        "Ok(rows.collect::<Result<Vec<_>, _>>()?)",
        "fn human_verdict_for_decision(decision: &str) -> AppResult<&'static str>",
        "fn rejected_transcript_for_learning(corrected: &str, candidates: &[Option<String>]) -> Option<String>",
        "SELECT COALESCE(is_gold, 0), raw_transcript, normalized_transcript, annotated_transcript,\n                        verdict_transcript",
        "verdict_transcript.clone(),",
        "Some(raw_transcript.clone()),",
    ]
    missing = [pattern for pattern in required if pattern not in db]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"db.rs is missing explicit DB read error propagation:\n{formatted}")
    if db.count("Ok(rows.collect::<Result<Vec<_>, _>>()?)") < 2:
        raise AssertionError("db.rs must propagate row-mapping errors from batch segment and escalation reads")
    if "): HumanDecisionContext =" not in db:
        raise AssertionError("Database::record_human_decision must read a full segment snapshot before updating it")
    if "human_edit_learning_uses_agent_proposal_before_raw_asr" not in db:
        raise AssertionError("Database::record_human_decision needs a regression for agent proposal learning examples")


def test_database_fts_maintenance_does_not_silently_discard_errors() -> None:
    db = db_surface()
    forbidden = [
        'self.conn.execute_batch("INSERT INTO segments_fts(segments_fts) VALUES(\'rebuild\');").ok();',
        'self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES(\'optimize\')", []).ok();',
        # vacuum()'s rebuild must NOT bail with the bare rusqlite error — VACUUM already committed and
        # renumbering desync (if any) self-heals on restart, so the failure must say so, not surface a
        # cryptic message over a possibly-stale search index.
        'self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES(\'rebuild\')", [])?;',
    ]
    present = [pattern for pattern in forbidden if pattern in db]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"db.rs silently discards FTS maintenance errors:\n{formatted}")

    required = [
        'self.conn.execute_batch("INSERT INTO segments_fts(segments_fts) VALUES(\'rebuild\');")?;',
        'tracing::warn!("Failed to optimize segments FTS index after batch delete: {error}");',
        "fn fts_index_searches_inserted_segments_and_tracks_batch_delete()",
        # vacuum() surfaces an actionable rebuild-failure message (restart repairs the index).
        "VACUUM completed but rebuilding the search index failed:",
        "fn vacuum_rebuilds_fts_and_leaves_search_working()",
    ]
    missing = [pattern for pattern in required if pattern not in db]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"db.rs must keep observable FTS maintenance handling:\n{formatted}")


def test_database_savepoint_cleanup_reports_failures() -> None:
    db = db_surface()
    forbidden = [
        'let _ = self.conn.execute("ROLLBACK TO batch_insert", []);',
        'let _ = self.conn.execute("RELEASE batch_insert", []);',
        'let _ = self.conn.execute("ROLLBACK TO merge_json", []);',
        'let _ = self.conn.execute("RELEASE merge_json", []);',
        'let _ = self.conn.execute("ROLLBACK TO batch_delete", []);',
        'let _ = self.conn.execute("RELEASE batch_delete", []);',
        'let _ = self.conn.execute("ROLLBACK TO consensus_batch", []);',
        'let _ = self.conn.execute("RELEASE consensus_batch", []);',
    ]
    present = [pattern for pattern in forbidden if pattern in db]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"db.rs silently discards savepoint cleanup failures:\n{formatted}")

    required = [
        "fn cleanup_savepoint_after_error(&self, savepoint: &str)",
        'tracing::warn!("Failed to roll back savepoint {savepoint}: {error}");',
        'tracing::warn!("Failed to release savepoint {savepoint}: {error}");',
        'self.cleanup_savepoint_after_error("batch_insert");',
        'self.cleanup_savepoint_after_error("merge_json");',
        'self.cleanup_savepoint_after_error("batch_delete");',
        'self.cleanup_savepoint_after_error("consensus_batch");',
    ]
    missing = [pattern for pattern in required if pattern not in db]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"db.rs must keep observable savepoint cleanup handling:\n{formatted}")


def test_atomic_file_cleanup_reports_failures() -> None:
    atomic_file = (REPO_ROOT / "src-tauri/src/atomic_file.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = fs::rename(&backup_path, final_path);",
        "let _ = fs::remove_file(path);",
    ]
    present = [pattern for pattern in forbidden if pattern in atomic_file]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"atomic_file.rs silently discards cleanup failures:\n{formatted}")

    required = [
        "Failed to restore {} from replacement backup {} after replace error: {restore_err}",
        "Failed to remove temporary file {} after error: {e}",
        "Err(e) if e.kind() == io::ErrorKind::NotFound => {}",
    ]
    missing = [pattern for pattern in required if pattern not in atomic_file]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"atomic_file.rs must keep observable cleanup failure handling:\n{formatted}")


def test_model_artifact_cleanup_reports_failures() -> None:
    models = (REPO_ROOT / "src-tauri/src/models.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = std::fs::remove_file(path);",
        "let _ = fs::remove_file(&tmp_archive);",
        "let _ = fs::remove_file(&tmp);",
        "let _ = fs::remove_file(tmp_path);",
        "let _ = fs::remove_file(tmp);",
    ]
    present = [pattern for pattern in forbidden if pattern in models]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"models.rs silently discards model artifact cleanup failures:\n{formatted}")

    required = [
        "fn remove_model_temp_file(path: &Path, context: &str)",
        'remove_model_temp_file(path, "unpinned model download");',
        'remove_model_temp_file(path, "SHA256-mismatched model download");',
        'remove_model_temp_file(&tmp_archive, "undersized OmniASR archive");',
        'remove_model_temp_file(&tmp_archive, "completed OmniASR archive");',
        'remove_model_temp_file(&tmp, "failed model extraction temp");',
        'remove_model_temp_file(&tmp, "failed token extraction temp");',
        # CAM++/denoiser are direct .onnx downloads now (no tar.bz2 to clean after extraction); their
        # temp is cleaned on failure by write_reader_to_temp (read error) and verify_sha256
        # (mismatch/unpinned — see the "SHA256-mismatched"/"unpinned" entries above), promoted by
        # replace_file on success. No dedicated archive-completion cleanup call remains for them.
        'remove_model_temp_file(&tmp, "undersized model download");',
        'remove_model_temp_file(tmp_path, "partial model download");',
        'remove_model_temp_file(tmp, "staged model extraction temp");',
        'tracing::warn!("Failed to remove {context} file {}: {error}", path.display()),',
    ]
    missing = [pattern for pattern in required if pattern not in models]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"models.rs must keep observable model artifact cleanup handling:\n{formatted}")


def test_model_metadata_updates_do_not_silently_default() -> None:
    models = (REPO_ROOT / "src-tauri/src/models.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = fs::create_dir_all(parent);",
        "self.load_meta().unwrap_or_default()",
        "compute_file_sha256(&path).unwrap_or_default()",
    ]
    present = [pattern for pattern in forbidden if pattern in models]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"models.rs silently defaults model metadata or directory setup failures:\n{formatted}")

    required = [
        "fn load_meta_for_update(&self) -> Vec<ModelMeta>",
        'tracing::warn!("Failed to parse existing model metadata before update; starting fresh: {error}");',
        'tracing::warn!("Failed to read existing model metadata before update; starting fresh: {error}");',
        "let mut meta_entries = self.load_meta_for_update();",
        "let sha256 = compute_file_sha256(&path)?;",
        "fn ensure_model_parent_dir(path: &Path)",
        'tracing::warn!("Failed to create model artifact parent directory {}: {error}", parent.display());',
        "fn load_meta_for_update_treats_missing_as_empty_but_surfaces_corrupt_to_strict_loader()",
    ]
    missing = [pattern for pattern in required if pattern not in models]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"models.rs must keep observable model metadata update handling:\n{formatted}")

    if models.count("let mut meta_entries = self.load_meta_for_update();") != 2:
        raise AssertionError("models.rs must use load_meta_for_update() for both OmniASR and direct model metadata updates")


def test_audio_decode_worker_send_failures_are_reported() -> None:
    audio = (REPO_ROOT / "src-tauri/src/audio.rs").read_text(encoding="utf-8")
    forbidden = [
        "let _ = tx.send(result);",
    ]
    present = [pattern for pattern in forbidden if pattern in audio]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"audio.rs silently discards decode worker send failures:\n{formatted}")

    required = [
        "fn send_decode_worker_result<T>(",
        'send_decode_worker_result(tx, result, "decode_pcm_windows");',
        'send_decode_worker_result(tx, result, "decode_to_pcm");',
        'tracing::warn!("Audio decode worker could not send {operation} result; receiver was dropped or timed out");',
    ]
    missing = [pattern for pattern in required if pattern not in audio]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"audio.rs must keep observable decode worker send handling:\n{formatted}")


def test_audio_decode_reset_required_fails_loud_not_silent_truncation() -> None:
    """Both decode loops (decode_to_pcm, decode_pcm_windows) can hit next_packet()'s ResetRequired on a
    chained/multi-stream file MID-stream — by definition there is more audio after that point. The old arm
    logged a tracing::warn! and `break`, keeping only the decoded PREFIX and returning Ok: a silent-
    truncation data-loss trap (same class as the fixed `Err(_) => break`) that a GUI import never surfaces.
    Both arms must fail LOUDLY, matching the corrupt-source arm right beside them. Regression guard: iter-152."""
    audio = (REPO_ROOT / "src-tauri/src/audio.rs").read_text(encoding="utf-8")
    forbidden = "stream reset required mid-file; stopping decode early"
    if forbidden in audio:
        raise AssertionError(
            "audio.rs still silently breaks on a mid-stream ResetRequired (drops the tail of a legitimate "
            f"chained/multi-stream file):\n- {forbidden}"
        )
    loud = "decode stopped at a mid-stream reset (chained or multi-stream file)."
    count = audio.count(loud)
    if count != 2:
        raise AssertionError(
            "audio.rs must fail loudly on ResetRequired in BOTH decode loops (decode_to_pcm + "
            f"decode_pcm_windows); found {count} of 2 loud handlers."
        )


def test_pipeline_wsl_subprocess_send_failures_are_reported() -> None:
    pipeline = pipeline_surface()
    forbidden = [
        "let _ = tx.send(cmd.output());",
        "let _ = child.kill();",
        "let _ = child.wait();",
        "stdout_reader.join().unwrap_or_default()",
        "stderr_reader.join().unwrap_or_default()",
    ]
    present = [pattern for pattern in forbidden if pattern in pipeline]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"pipeline.rs silently discards WSL subprocess send failures:\n{formatted}")

    required = [
        "fn kill_and_reap_wsl_child(child: &mut std::process::Child, context: &str)",
        "kill_and_reap_wsl_child(&mut child, \"timed-out WSL subprocess\");",
        "kill_and_reap_wsl_child(&mut child, \"failed WSL subprocess\");",
        "fn join_wsl_pipe_reader(thread: std::thread::JoinHandle<Vec<u8>>, stream: &str) -> Vec<u8>",
        'tracing::warn!("WSL subprocess {stream} reader panicked");',
    ]
    missing = [pattern for pattern in required if pattern not in pipeline]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"pipeline.rs must keep observable WSL subprocess send handling:\n{formatted}")


def test_wsl_import_pass_does_not_roll_back_the_file_on_a_benign_empty_result() -> None:
    """run_primary_wsl_pass_for_import calls self.transcribe(), which converts a legit-but-EMPTY 7B result
    (a silent/music/noise chunk — parse_wsl_segment_result returns Ok("")) into an Err so the re-transcribe
    IPCs never blank-overwrite a stored transcript. But the IMPORT pass also routes through transcribe(), and
    for it an empty result is NOT an infra failure — the Ok-arm usable=false path already escalates only that
    one segment. If the Err arm can't tell the empty-result Err from a real "server down" Err, it sets
    infra_failure=true and delete_segments_batch rolls back the ENTIRE file (discarding the good transcripts of
    its other chunks) on the owner's PRIMARY import engine, and reports a false "server is not running". The
    Err arm MUST route the WSL_7B_EMPTY_RESULT_MARKER error to the non-infra escalate path. Runtime needs a
    full pipeline + WSL server + a non-speech clip, so source-pinned (found by adversarial hunt-8)."""
    pipeline = pipeline_surface()
    if 'WSL_7B_EMPTY_RESULT_MARKER: &str = "WSL 7B returned an empty transcript"' not in pipeline:
        raise AssertionError("the WSL_7B_EMPTY_RESULT_MARKER constant (produced by transcribe's empty-result Err) is gone")
    if "contains(WSL_7B_EMPTY_RESULT_MARKER)" not in pipeline:
        raise AssertionError(
            "the WSL-7B import pass no longer distinguishes a benign EMPTY 7B result from an infra failure — "
            "one silent/music/noise chunk would set infra_failure=true and roll back the WHOLE file (discarding "
            "its other chunks' good transcripts). The Err arm must check `msg.contains(WSL_7B_EMPTY_RESULT_MARKER)`"
            " and route an empty result to the non-infra escalate path (infra_failure=false)."
        )


def test_probe_wsl_7b_server_reaps_child_on_wait_error() -> None:
    """probe_wsl_7b_server polls child.try_wait() in a loop; on a wait-status error its Err arm must
    reap the child (kill_and_reap_wsl_child) before returning false — exactly like the sibling
    try_wait loops (run_wsl_segment_transcript and the 7B preflight probe both reap on their Err
    arm). std::process::Child does NOT kill/reap on drop, so a bare `return false` on the Err arm
    leaks a WSL process on every failed status poll, and this probe is called on a poll.
    Regression guard: iter-65."""
    pipeline = (REPO_ROOT / "src-tauri" / "src" / "pipeline.rs").read_text(encoding="utf-8")
    marker = "fn probe_wsl_7b_server(timeout_secs: u64) -> bool {"
    start = pipeline.find(marker)
    if start == -1:
        raise AssertionError("probe_wsl_7b_server not found — this gate would pass vacuously")
    # Scope the check to this function body: slice to the start of the next free function.
    rest = pipeline[start + len(marker):]
    end = rest.find("\nfn ")
    next_pub = rest.find("\npub(crate) fn ")
    if next_pub != -1 and (end == -1 or next_pub < end):
        end = next_pub
    body = rest if end == -1 else rest[:end]

    # Forbidden: a try_wait Err arm that returns without reaping.
    if "Err(_) => return false" in body:
        raise AssertionError(
            "probe_wsl_7b_server's try_wait Err arm returns false without reaping the child — it "
            "leaks a WSL process on every failed status poll. Reap via kill_and_reap_wsl_child like "
            "the sibling loops."
        )
    # Required: reap on BOTH the deadline branch AND the try_wait Err branch.
    reaps = body.count('kill_and_reap_wsl_child(&mut child, "engine-status probe");')
    if reaps < 2:
        raise AssertionError(
            "probe_wsl_7b_server must reap the child on both the timeout branch and the try_wait Err "
            f"branch (found {reaps} reap call(s); expected >= 2)"
        )


def test_aligner_score_consistency_caps_clip_and_dp() -> None:
    """score_consistency runs the ONNX forward pass + a num_frames×num_states forward-backward on the
    WHOLE clip (a whole-recording segment reaches it via slice_pcm_by_alignment's None fallback).
    Without align()'s MAX_ALIGN_SECS duration cap AND a MAX_VITERBI_CELLS DP cap it OOM-aborts the
    scoring worker (round-24 hunt #17). This path needs the real ONNX model so it is not
    unit-testable; guard the two caps at the source. Scoped to the score_consistency function body."""
    aligner = (REPO_ROOT / "src-tauri" / "src" / "aligner.rs").read_text(encoding="utf-8")
    marker = "pub fn score_consistency("
    start = aligner.find(marker)
    if start == -1:
        raise AssertionError("score_consistency not found — this gate would pass vacuously")
    rest = aligner[start + len(marker):]
    # score_consistency is the last method in the impl, so scope to its OWN closing brace: the first
    # 4-space-indented `}` (inner blocks close at deeper indentation), not the next fn (the fns that
    # follow are module-level `fn`, which a `\n    fn ` marker would miss — the body would then bleed
    # into ctc_logits_to_word_timestamps and inherit its MAX_VITERBI_CELLS).
    end = rest.find("\n    }")
    body = rest if end == -1 else rest[:end]
    required = [
        "MAX_ALIGN_SECS",  # duration cap before the ONNX forward pass
        "MAX_VITERBI_CELLS",  # DP cell cap before forward_backward_ctc_score
    ]
    missing = [pattern for pattern in required if pattern not in body]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"score_consistency must cap clip duration AND DP size before running:\n{formatted}")


def test_finish_import_clears_cancel_token_before_opening_the_gate() -> None:
    """finish_import and finish_batch both share a round-15 TOCTOU: if the state gate flips to Idle
    BEFORE the (possibly-cancelled) cancel token is cleared, a new operation can start in that window,
    arm its own token, and then have finish's second statement wipe that fresh token — leaving the new
    run uncancellable. finish_batch clears the token first; finish_import must too. This is a
    concurrency-ordering invariant (single-threaded both orders behave identically, so it is not
    unit-testable) — enforce the order at the source, scoped to each function body."""
    lib = (REPO_ROOT / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    for fn_name, token_line, gate_line in (
        ("pub fn finish_import(", "lock_import_cancel_token() = None", "ImportState::Idle"),
        ("pub fn finish_batch(", "lock_batch_cancel_token() = None", "BatchState::Idle"),
    ):
        start = lib.find(fn_name)
        if start == -1:
            raise AssertionError(f"{fn_name} not found — this gate would pass vacuously")
        rest = lib[start + len(fn_name):]
        end = rest.find("\n    }")
        body = rest if end == -1 else rest[:end]
        token_at = body.find(token_line)
        gate_at = body.find(gate_line)
        if token_at == -1 or gate_at == -1:
            raise AssertionError(f"{fn_name} no longer clears the token and/or opens the gate as expected")
        if token_at > gate_at:
            raise AssertionError(
                f"{fn_name} opens the state gate BEFORE clearing the cancel token — a new operation can "
                f"start in the window and lose its token (round-15 TOCTOU). Clear the token first."
            )


def test_batch_normalize_uses_a_targeted_update_not_a_whole_row_upsert() -> None:
    """batch_normalize must persist normalized_transcript with the targeted single-column
    update_normalized_transcript, NOT a read-modify-write + whole-row insert_segment upsert of a
    re-read segment — the latter clobbers a concurrent write to the same segment that lands between
    the re-read and the upsert (iter-88; the sibling batch commands verify/assign_speaker already use
    targeted updates). Scoped to the batch_normalize function body."""
    batch = (REPO_ROOT / "src-tauri" / "src" / "commands" / "batch.rs").read_text(encoding="utf-8")
    marker = "pub fn batch_normalize("
    start = batch.find(marker)
    if start == -1:
        raise AssertionError("batch_normalize not found — this gate would pass vacuously")
    body = batch[start:]
    if "insert_segment(" in body:
        raise AssertionError(
            "batch_normalize uses a whole-row insert_segment upsert — a concurrent write to the segment "
            "can be clobbered; use the targeted db.update_normalized_transcript instead."
        )
    if "update_normalized_transcript(" not in body:
        raise AssertionError("batch_normalize must persist via the targeted db.update_normalized_transcript")


def test_save_session_is_rate_limited() -> None:
    """save_session is a webview-reachable WRITE — a DB session upsert taken under the GLOBAL db lock —
    so, like every other IPC command in infra.rs, it must throttle through the rate limiter. The
    frontend debounces it to ~1/800ms (so this never rejects a legitimate save), but without a limiter a
    webview loop that bypasses that debounce can pin the db lock and starve get_segments et al. (same
    'lone command missing a rate-limiter, a local DoS gap' class as export_audio round-22 #5 /
    register_media_asset round-25 #7). Scoped to the save_session function body — the next command
    (restore_session) has its OWN check that would otherwise satisfy this gate vacuously."""
    infra = (REPO_ROOT / "src-tauri" / "src" / "commands" / "infra.rs").read_text(encoding="utf-8")
    marker = "pub fn save_session("
    start = infra.find(marker)
    if start == -1:
        raise AssertionError("save_session not found — this gate would pass vacuously")
    rest = infra[start:]
    nxt = rest.find("#[tauri::command]")
    body = rest if nxt == -1 else rest[:nxt]
    # A STRICT_RATE_LIMITER.check("save_session") superstring would satisfy this too (either limiter is
    # acceptable throttling); the substring match accepts both.
    if 'RATE_LIMITER.check("save_session")' not in body:
        raise AssertionError(
            'save_session must throttle through RATE_LIMITER.check("save_session") — it is a '
            "webview-reachable DB write under the global db lock and was the lone infra.rs command "
            "without a rate limiter (local DoS gap)."
        )


def test_atomic_replace_post_swap_backup_cleanup_is_best_effort() -> None:
    """On Windows, atomic_file::replace_file moves the current file aside to a `.replace-bak`, promotes
    the tmp file (the durable swap), fsyncs the dir, THEN deletes the backup. Deleting that throwaway
    backup is non-load-bearing (recover_interrupted_replace only promotes a backup when the target is
    MISSING; the next replace_file removes any leftover). A transient Windows scanner lock (Defender /
    Search Indexer holding the fresh .replace-bak without FILE_SHARE_DELETE) makes remove_file fail with
    ERROR_SHARING_VIOLATION — and if that error is PROPAGATED it reports a SUCCEEDED, durable write as
    FAILED (a dishonest 'save failed' surfaced to the user). The post-swap cleanup must therefore be
    best-effort (log + Ok), like the fsync_parent_dir above it. This can't be unit-injected (the backup
    path is internal + pid-based), so it is pinned here. Scoped to the tail of the Windows replace_file
    (after the last fsync_parent_dir call), NOT the pre-swap cleanup at 39-43 which correctly propagates
    (there the swap never started and the file is unchanged)."""
    atomic = (REPO_ROOT / "src-tauri" / "src" / "atomic_file.rs").read_text(encoding="utf-8")
    marker = "fsync_parent_dir(final_path);"
    idx = atomic.rfind(marker)
    if idx == -1:
        raise AssertionError("atomic_file.rs: fsync_parent_dir call not found — this gate would pass vacuously")
    # Bound to the Windows replace_file tail: everything from the last fsync_parent_dir call up to the
    # next `#[cfg(target_os = "windows")]` (the fsync_parent_dir fn's own attribute).
    end = atomic.find('#[cfg(target_os = "windows")]', idx)
    tail = atomic[idx:] if end == -1 else atomic[idx:end]
    if "remove_file(&backup_path)" not in tail:
        raise AssertionError("atomic_file.rs post-swap tail no longer removes the backup — gate would pass vacuously")
    if "Err(e) => Err(e)" in tail:
        raise AssertionError(
            "atomic_file::replace_file propagates a POST-SWAP backup-cleanup error — a transient Windows "
            "lock on the throwaway .replace-bak then reports a SUCCEEDED, durable write as failed. Make the "
            "post-swap cleanup best-effort (log + Ok), like fsync_parent_dir."
        )
    if "tracing::warn!" not in tail:
        raise AssertionError("atomic_file::replace_file post-swap backup cleanup must log its failure best-effort")


def test_t0_calibration_excludes_human_rejected_from_the_conformal_set() -> None:
    """The T0 auto-accept conformal threshold is calibrated over `all_verified` (jury/mod.rs). A human-
    REJECTED clip is verified=true (set only to leave the review queue) with its annotated_transcript
    intact, so without an is_human_rejected filter its (nonconformity score, committed-CER) contaminates
    the per-SNR-bucket threshold that gates auto-accept WITHOUT human review — voiding the conformal
    coverage guarantee. Every export/gate path drops these rows via is_human_rejected; the calibration set
    must too. This is the 7th instance of the "rejected/empty counted as real" class and can't be
    unit-injected cheaply (a full jury run), so it is source-pinned. Scoped to the all_verified statement."""
    jury = (REPO_ROOT / "src-tauri" / "src" / "jury" / "mod.rs").read_text(encoding="utf-8")
    idx = jury.find("let all_verified")
    if idx == -1:
        raise AssertionError("jury/mod.rs `all_verified` not found — this gate would pass vacuously")
    stmt = jury[idx : jury.index(";", idx)]
    if "is_human_rejected" not in stmt:
        raise AssertionError(
            "jury/mod.rs `all_verified` (the T0 conformal calibration set) must exclude is_human_rejected "
            "clips — a mark-bad clip is verified=true and would contaminate the auto-accept threshold."
        )


def test_run_t2_for_segment_respects_the_autonomy_dial() -> None:
    """run_t2_for_segment (the direct "re-run T2 from the Review Inbox" IPC command) writes a machine
    jury_accept verdict to the DB. Like the pipeline chokepoint run_jury_pipeline_core_via, that machine
    commit MUST be gated behind the Autonomy Dial (machine_commits_allowed = ActConfirm | ActAuto) — under
    Observe/Propose (the shipped default) a machine verdict is staged for the human, never auto-committed.
    The command previously routed around the dial and silently accepted a machine transcript. Cloud-calling
    command (can't be unit-injected), so source-pinned. Scoped to the run_t2_for_segment function body."""
    jury = (REPO_ROOT / "src-tauri" / "src" / "commands" / "jury.rs").read_text(encoding="utf-8")
    start = jury.find("pub async fn run_t2_for_segment")
    if start == -1:
        raise AssertionError("run_t2_for_segment not found — this gate would pass vacuously")
    rest = jury[start:]
    nxt = rest.find("\n#[tauri::command]", 1)
    body = rest if nxt == -1 else rest[:nxt]
    if "write_segment_verdict" not in body:
        raise AssertionError("run_t2_for_segment no longer writes a verdict — gate would pass vacuously")
    if "machine_commits_allowed" not in body or "jury_autonomy_level" not in body:
        raise AssertionError(
            "run_t2_for_segment writes a machine jury_accept verdict WITHOUT gating on the Autonomy Dial "
            "(machine_commits_allowed / jury_autonomy_level) — under Observe/Propose it would auto-commit a "
            "machine transcript the dial forbids. Gate the write like run_jury_pipeline_core_via."
        )


def test_optin_transcribe_commands_reject_a_blank_decode() -> None:
    """transcribe_segment_constrained and transcribe_segment_finetuned run an opt-in engine and return its
    text to the frontend, which OVERWRITES the segment's transcript with it. Both callees return an empty
    string on a silent/blank-decoding clip (run_constrained -> ""; transcribe_chunk_finetuned -> Ok("")),
    so shipping it destroys an existing good transcript and persists a blank as a real result — the
    no-blank-transcript honesty rule + data loss. Each must reject a blank decode (return Err) before
    returning the text. Cloud/ONNX commands (can't be unit-injected without the model), so source-pinned."""
    src = (REPO_ROOT / "src-tauri" / "src" / "commands" / "transcribe.rs").read_text(encoding="utf-8")
    for fn in ("transcribe_segment_constrained", "transcribe_segment_finetuned"):
        start = src.find(f"pub async fn {fn}")
        if start == -1:
            raise AssertionError(f"{fn} not found — this gate would pass vacuously")
        rest = src[start:]
        nxt = rest.find("\n#[tauri::command]", 1)
        body = rest if nxt == -1 else rest[:nxt]
        if "text.trim().is_empty()" not in body:
            raise AssertionError(
                f"{fn} returns its decode text without a blank guard — a silent clip's empty decode would "
                "overwrite the existing transcript with a blank. Reject a blank decode with an Err first."
            )


def test_pipeline_wsl_retranscribe_rejects_an_empty_result() -> None:
    """The in-pipeline WSL-7B branch of transcribe() is the twin of the opt-in transcribe commands guarded
    above: run_wsl_segment_transcript returns Ok("") on a TRANSIENT empty result (server up but under load
    — documented in-code, observed 1-of-3 under stress), so map_err(tag_7b_unavailable) does NOT catch it.
    Without an explicit empty guard, update_asr_transcript_if_unreviewed(&id, "", ...) overwrites a good,
    unverified stored transcript with "" — silent data loss on both re-transcribe entry points (per-segment
    IPC + batch_transcribe), unlike the import path which retries/escalates. The branch must reject an empty
    raw_transcript (return a tagged 7B-unavailable Err) BEFORE the DB write. transcribe() needs the WSL
    server + audio + DB, so it is not unit-injectable — source-pinned."""
    pipeline = pipeline_surface()
    anchor = "self.run_wsl_segment_transcript(&id, cancel).map_err(tag_7b_unavailable)?;"
    start = pipeline.find(anchor)
    if start == -1:
        raise AssertionError("run_wsl_segment_transcript call not found — this gate would pass vacuously")
    # Match the METHOD CALL (`.update_asr_transcript_if_unreviewed(`), not a prose mention of the name in
    # the guard's own comment — otherwise the search lands on the comment and excludes the guard below it.
    write = pipeline.find(".update_asr_transcript_if_unreviewed(", start)
    if write == -1:
        raise AssertionError("update_asr_transcript_if_unreviewed call not found after the WSL call — gate vacuous")
    between = pipeline[start:write]
    if "raw_transcript.trim().is_empty()" not in between or "return Err(tag_7b_unavailable(" not in between:
        raise AssertionError(
            "transcribe()'s WSL-7B branch writes raw_transcript without an empty guard between the "
            "run_wsl_segment_transcript call and update_asr_transcript_if_unreviewed — a transient empty 7B "
            "result (Ok(\"\")) would overwrite a good stored transcript with a blank. Reject an empty "
            "raw_transcript with a tagged 7B-unavailable Err before the write."
        )


def test_pipeline_duration_probe_failures_are_not_silent() -> None:
    pipeline = pipeline_surface()
    forbidden = [
        "audio::get_duration_ms(path).unwrap_or(0)",
        "let duration_ms = audio::get_duration_ms(path).unwrap_or(0);",
    ]
    present = [pattern for pattern in forbidden if pattern in pipeline]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"pipeline.rs silently defaults duration probe failures to zero:\n{formatted}")

    required = [
        "return self.process_single_file_streaming(path, db, decode_timeout, duration_ms, cancel, on_chunk);",
        "decode_timeout: Duration,\n        duration_ms: i64,",
        "let duration_ms = audio::get_duration_ms(path)?;",
        'tracing::warn!("Rediarize duration probe failed for {audio_path}: {error}");',
    ]
    missing = [pattern for pattern in required if pattern not in pipeline]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"pipeline.rs must propagate or report duration probe failures:\n{formatted}")

    if pipeline.count("let duration_ms = audio::get_duration_ms(path)?;") < 3:
        raise AssertionError("pipeline.rs must keep duration probe propagation on import, single-file import, and transcribe paths")


def test_export_bundle_model_metadata_load_errors_are_visible() -> None:
    # export_bundle.rs's #[cfg(test)] module was split into export_bundle_tests.rs via #[path]
    # (Week-4 decomposition). The FORBIDDEN check stays scoped to the PRODUCTION file only — a test may
    # legitimately mention the silent-discard pattern, and we must not false-positive on that. The
    # REQUIRED check pins two #[test]-side needles (the regression fn name + its assertion) that now live
    # in the tests file, so it scans the whole SURFACE, else the moved regression vanishes vacuously.
    export_bundle_prod = (REPO_ROOT / "src-tauri/src/export_bundle.rs").read_text(encoding="utf-8")
    surface_parts = [export_bundle_prod]
    bundle_tests = REPO_ROOT / "src-tauri/src/export_bundle_tests.rs"
    if bundle_tests.is_file():
        surface_parts.append(bundle_tests.read_text(encoding="utf-8"))
    export_bundle_surface = "\n".join(surface_parts)
    if "#[test]" not in export_bundle_surface or "fn draft_export" not in export_bundle_surface:
        raise AssertionError("export_bundle surface is missing test code — this panic gate would pass vacuously")

    forbidden = [
        "model_manager.load_meta().unwrap_or_default()",
    ]
    present = [pattern for pattern in forbidden if pattern in export_bundle_prod]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"export_bundle.rs silently hides model metadata load failures:\n{formatted}")

    required = [
        "fn load_model_metadata_for_manifest(model_manager: &ModelManager)",
        "let (model_meta, model_meta_load_error) = load_model_metadata_for_manifest(model_manager);",
        '"installedMetadataLoadError": model_meta_load_error,',
        'tracing::warn!("Failed to load model metadata for export bundle: {error}");',
        "fn draft_export_records_model_metadata_load_errors()",
        'model_manifest["installedMetadataLoadError"].as_str().unwrap().contains("Parse meta")',
    ]
    missing = [pattern for pattern in required if pattern not in export_bundle_surface]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"export_bundle surface must keep model metadata load failures visible:\n{formatted}")


def test_eval_read_paths_do_not_silently_drop_rows() -> None:
    eval_rs = (REPO_ROOT / "src-tauri/src/eval.rs").read_text(encoding="utf-8")
    required = [
        "pub fn list_gold_segments(db: &Database) -> AppResult<Vec<GoldSegment>>",
        "pub fn list_eval_runs(db: &Database) -> AppResult<Vec<EvalRun>>",
        "Ok(rows.collect::<Result<Vec<_>, _>>()?)",
    ]
    missing = [pattern for pattern in required if pattern not in eval_rs]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"eval.rs is missing explicit row error propagation:\n{formatted}")
    # Verify each listing propagates row-mapping errors WITHIN ITS OWN BODY. (A global
    # occurrence count is brittle: it false-fails the moment any other query fn correctly
    # adopts the same safe collect pattern — e.g. the IAA-triples listing — so check the two
    # required functions individually, which is strictly stronger.)
    collect_pattern = "Ok(rows.collect::<Result<Vec<_>, _>>()?)"
    for sig in (
        "pub fn list_gold_segments(db: &Database) -> AppResult<Vec<GoldSegment>>",
        "pub fn list_eval_runs(db: &Database) -> AppResult<Vec<EvalRun>>",
    ):
        start = eval_rs.index(sig)
        rest = eval_rs[start + len(sig):]
        end = len(rest)
        for marker in ("\npub fn ", "\nfn "):
            idx = rest.find(marker)
            if idx != -1:
                end = min(end, idx)
        if collect_pattern not in rest[:end]:
            fn_name = sig.split("(")[0].replace("pub fn ", "")
            raise AssertionError(
                f"eval.rs: {fn_name} must propagate row-mapping errors ({collect_pattern})"
            )


def test_pipeline_rediarize_reports_db_update_failures() -> None:
    # rediarize_segments() snapshots its segments up front, then spends a per-file decode + ONNX speaker
    # embedding pass (the decode timeout alone clamps up to 3600s) DELIBERATELY holding no AppState lock,
    # so concurrent human edits are expected BY DESIGN. It must therefore write the speaker back with the
    # TARGETED single-column update_speaker_id (db.rs documents it as "without touching any other field").
    # The previous whole-row insert_segment upsert of the stale snapshot silently reverted every column a
    # human changed during the pass and — since insert_segment is an UPSERT — resurrected segments deleted
    # mid-pass. Same anti-clobber discipline as batch transcribe above. The write outcome must still never
    # be swallowed, which is what this gate originally existed to enforce.
    pipeline = pipeline_surface()
    required = [
        "pub fn rediarize_segments(&self, ids: &[String]) -> AppResult<usize>",
        "let db = self.open_db()?;",
        "match db.update_speaker_id(seg_id, Some(label.as_str()))",
        'tracing::error!("Rediarize speaker update failed for {seg_id}: {error}")',
        # A row deleted during the long pass is a no-op, never a revival.
        'tracing::warn!("Rediarize speaker update skipped: segment {seg_id} no longer exists")',
    ]
    missing = [pattern for pattern in required if pattern not in pipeline]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"pipeline.rs is missing explicit rediarization DB update handling:\n{formatted}")
    # And the stale whole-row upsert must not come back at this site.
    if "seg.speaker_id = Some(label);" in pipeline:
        raise AssertionError(
            "pipeline.rs rediarize is mutating a stale segment snapshot again — that is the whole-row "
            "clobber this gate exists to prevent; use db.update_speaker_id(id, ...) instead"
        )


def test_file_dialog_commands_do_not_block_the_main_thread() -> None:
    """Regression gate for the 2026-07-11 crash: open_audio_file / import_directory were SYNC
    #[tauri::command]s calling blocking_pick_file()/blocking_pick_folder(). Sync commands run on the
    MAIN THREAD, so the blocking native picker froze the ENTIRE app UI while open (confirmed: a second
    command hung the full timeout while the dialog was up). Both MUST stay `async fn` and use the
    non-blocking pick_file/pick_folder callback form so the dialog never blocks the event loop.
    """
    commands = command_surface()

    def body_of(sig: str) -> str:
        start = commands.index(sig)
        rest = commands[start + len(sig):]
        end = len(rest)
        for marker in ("\n#[tauri::command]", "\npub fn ", "\npub async fn "):
            idx = rest.find(marker)
            if idx != -1:
                end = min(end, idx)
        return rest[:end]

    for sig in ("pub async fn open_audio_file", "pub async fn import_directory"):
        if sig not in commands:
            raise AssertionError(
                f"commands.rs: '{sig}' must exist and be ASYNC (a sync dialog command blocks the "
                f"main thread and freezes the whole UI — see the 2026-07-11 crash)."
            )
        body = body_of(sig)
        if "blocking_pick" in body:
            raise AssertionError(
                f"commands.rs: {sig} calls a blocking_pick_* dialog on the main thread — it MUST use "
                f"the non-blocking pick_file/pick_folder callback form or it freezes the app UI."
            )
        if "pick_file(" not in body and "pick_folder(" not in body:
            raise AssertionError(f"commands.rs: {sig} must open its dialog via non-blocking pick_file/pick_folder.")


def test_batch_processor_asr_errors_are_not_blank_transcripts() -> None:
    batch = (REPO_ROOT / "src-tauri/src/bin/batch_processor.rs").read_text(encoding="utf-8")
    required = [
        "let asr_result: Result<(String, Option<f64>, cortex_speech_app_lib::asr::ConfidenceSource), String>",
        "ASR service is unavailable; models may not be downloaded",
        "ASR transcription failed for segment",
        "return Err(std::io::Error::other(error).into());",
    ]
    missing = [pattern for pattern in required if pattern not in batch]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"batch_processor.rs is missing explicit ASR failure handling:\n{formatted}")


def test_pipeline_hypothesis_population_reports_failures() -> None:
    pipeline = pipeline_surface()
    required = [
        "fn log_hypothesis_population_failure(segment_id: &str, error: &AppError)",
        "fn insert_hypothesis_checked(",
        "Failed to insert {model_id} hypothesis for {segment_id}: {error}",
        "hypothesis transcription failed for {segment_id}: {error}",
        "hypothesis model unavailable for {segment_id}",
        "Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE))",
        # GER context loads are best-effort (refining unprimed is legitimate) but a DB READ FAILURE must
        # be logged — the old unwrap_or_default() made a persistent DB problem silently produce unprimed
        # GER forever with no trace.
        "GER: could not load N-best hypotheses for {id}: {e}; refining unprimed",
        "GER: could not load few-shot corrections for {id}: {e}; refining unprimed",
    ]
    missing = [pattern for pattern in required if pattern not in pipeline]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"pipeline.rs is missing explicit hypothesis failure handling:\n{formatted}")


def test_asr_pool_recovers_poisoned_state_lock() -> None:
    asr = (REPO_ROOT / "src-tauri/src/asr.rs").read_text(encoding="utf-8")
    if "fn lock_state(&self) -> MutexGuard<'_, AsrPoolState>" not in asr:
        raise AssertionError("AsrPool must centralize state locking behind lock_state()")
    if "Recovering poisoned ASR pool state lock" not in asr:
        raise AssertionError("AsrPool must warn when recovering a poisoned state lock")
    if "poisoned.into_inner()" not in asr:
        raise AssertionError("AsrPool must recover poisoned state locks with poisoned.into_inner()")
    direct_lock_count = asr.count("self.inner.lock()")
    if direct_lock_count != 1:
        raise AssertionError(f"self.inner.lock() must only appear inside AsrPool::lock_state(), found {direct_lock_count}")
    if "asr_pool_recovers_poisoned_state_lock" not in asr:
        raise AssertionError("asr.rs must keep a unit test for poisoned ASR pool-state recovery")


# NOTE: test_ood_session_recovers_poisoned_lock was intentionally removed. The OOD detector no longer
# holds an ONNX session to lock: the fabricated WavLM / sine-wave-centroid OOD path was deleted for
# honesty (Round-24 — it scored OOD as distance to a synthetic sine wave) and replaced with a
# session-free signal-processing heuristic (ZCR + frame-energy variance) in quality/signal_anomaly.rs. There is no
# session lock left to poison-recover; re-asserting one here would force re-introducing the dishonest path.


def test_global_rate_limiter_recovers_poisoned_lock() -> None:
    throttle = (REPO_ROOT / "src-tauri/src/throttle.rs").read_text(encoding="utf-8")
    if "Recovering poisoned rate limiter lock" not in throttle:
        raise AssertionError("GlobalRateLimiter must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in throttle:
        raise AssertionError("GlobalRateLimiter must recover poisoned locks with poisoned.into_inner()")
    if "global_rate_limiter_recovers_poisoned_lock" not in throttle:
        raise AssertionError("throttle.rs must keep a unit test for poisoned rate-limiter recovery")


def test_audio_fingerprint_cache_recovers_poisoned_lock() -> None:
    fingerprint = (REPO_ROOT / "src-tauri/src/fingerprint.rs").read_text(encoding="utf-8")
    if "fn lock_known(&self) -> MutexGuard<'_, HashMap<u64, String>>" not in fingerprint:
        raise AssertionError("AudioFingerprint must centralize cache locking behind lock_known()")
    if "Recovering poisoned audio fingerprint cache" not in fingerprint:
        raise AssertionError("AudioFingerprint must warn when recovering a poisoned cache")
    if "poisoned.into_inner()" not in fingerprint:
        raise AssertionError("AudioFingerprint must recover poisoned cache locks with poisoned.into_inner()")
    if "duplicate_detection_recovers_poisoned_cache" not in fingerprint:
        raise AssertionError("fingerprint.rs must keep a unit test for poisoned cache recovery")


def test_transcript_cache_recovers_poisoned_lock_and_never_zero_capacity() -> None:
    cache = (REPO_ROOT / "src-tauri/src/cache.rs").read_text(encoding="utf-8")
    if "fn lock_store(&self) -> MutexGuard<'_, HashMap<String, CacheEntry>>" not in cache:
        raise AssertionError("TranscriptCache must centralize store locking behind lock_store()")
    if "Recovering poisoned transcript cache" not in cache:
        raise AssertionError("TranscriptCache must warn when recovering a poisoned store")
    if "poisoned.into_inner()" not in cache:
        raise AssertionError("TranscriptCache must recover poisoned store locks with poisoned.into_inner()")
    if "let max_entries = max_entries.max(1);" not in cache:
        raise AssertionError("TranscriptCache must clamp max_entries to at least one")
    if "cache_recovers_poisoned_store" not in cache:
        raise AssertionError("cache.rs must keep a unit test for poisoned store recovery")
    if "zero_capacity_cache_keeps_one_entry" not in cache:
        raise AssertionError("cache.rs must keep a unit test for zero-capacity cache behavior")


def test_memoizer_recovers_poisoned_lock_and_never_zero_capacity() -> None:
    perf = (REPO_ROOT / "src-tauri/src/perf/mod.rs").read_text(encoding="utf-8")
    if "fn lock_cache(&self) -> MutexGuard<'_, LruCache<K, V>>" not in perf:
        raise AssertionError("Memoizer must centralize cache locking behind lock_cache()")
    if "Recovering poisoned memoizer cache" not in perf:
        raise AssertionError("Memoizer must warn when recovering a poisoned cache")
    if "poisoned.into_inner()" not in perf:
        raise AssertionError("Memoizer must recover poisoned cache locks with poisoned.into_inner()")
    if "NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN)" not in perf:
        raise AssertionError("Memoizer must construct non-zero capacity without unwrap()")
    if "test_memoizer_recovers_poisoned_cache" not in perf:
        raise AssertionError("perf/mod.rs must keep a unit test for poisoned memoizer recovery")
    if "test_memoizer_zero_capacity_keeps_one_entry" not in perf:
        raise AssertionError("perf/mod.rs must keep a unit test for zero-capacity memoizer behavior")


def test_history_manager_recovers_poisoned_stacks() -> None:
    history = (REPO_ROOT / "src-tauri/src/history/mod.rs").read_text(encoding="utf-8")
    if "fn lock_undo_stack(&self) -> MutexGuard<'_, VecDeque<Command>>" not in history:
        raise AssertionError("HistoryManager must centralize undo locking behind lock_undo_stack()")
    if "fn lock_redo_stack(&self) -> MutexGuard<'_, VecDeque<Command>>" not in history:
        raise AssertionError("HistoryManager must centralize redo locking behind lock_redo_stack()")
    if "Recovering poisoned undo history stack" not in history:
        raise AssertionError("HistoryManager must warn when recovering a poisoned undo stack")
    if "Recovering poisoned redo history stack" not in history:
        raise AssertionError("HistoryManager must warn when recovering a poisoned redo stack")
    if "poisoned.into_inner()" not in history:
        raise AssertionError("HistoryManager must recover poisoned stack locks with poisoned.into_inner()")
    if "history_operations_recover_poisoned_stacks" not in history:
        raise AssertionError("history/mod.rs must keep a unit test for poisoned stack recovery")


def test_inference_metrics_recover_poisoned_locks() -> None:
    inference = (REPO_ROOT / "src-tauri/src/inference.rs").read_text(encoding="utf-8")
    if "fn lock_latencies<'a>(latencies: &'a Mutex<Vec<f64>>, label: &str) -> MutexGuard<'a, Vec<f64>>" not in inference:
        raise AssertionError("InferenceMetrics must centralize latency locking behind lock_latencies()")
    if "Recovering poisoned {label} inference latency metrics" not in inference:
        raise AssertionError("InferenceMetrics must warn when recovering poisoned latency metrics")
    if "Recovering poisoned model-load inference metric" not in inference:
        raise AssertionError("InferenceMetrics must warn when recovering poisoned model-load metric")
    if inference.count("poisoned.into_inner()") < 2:
        raise AssertionError("InferenceMetrics must recover both latency and model-load locks")
    if "pub fn set_model_load_time_ms(&self, value: f64)" not in inference:
        raise AssertionError("InferenceMetrics must expose set_model_load_time_ms() for startup timing")
    if "inference_metrics_recover_poisoned_latency_lock" not in inference:
        raise AssertionError("inference.rs must keep a unit test for poisoned latency metric recovery")
    if "inference_metrics_recover_poisoned_model_load_lock" not in inference:
        raise AssertionError("inference.rs must keep a unit test for poisoned model-load metric recovery")
    lib = (REPO_ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
    if "INFERENCE_METRICS.set_model_load_time_ms" not in lib:
        raise AssertionError("lib.rs startup warmup timing must use set_model_load_time_ms()")


def test_pcm_cache_recovers_poisoned_lock() -> None:
    audio = (REPO_ROOT / "src-tauri/src/audio.rs").read_text(encoding="utf-8")
    if "fn pcm_cache_capacity() -> NonZeroUsize" not in audio:
        raise AssertionError("audio.rs must keep PCM cache capacity construction behind pcm_cache_capacity()")
    if "fn pcm_cache_key(path: &Path) -> AppResult<String>" not in audio:
        raise AssertionError("PCM decode cache must build keys from audio content, not only file paths")
    if "blake3::Hasher::new()" not in audio:
        raise AssertionError("PCM decode cache key must use a stable content hash")
    if "NonZeroUsize::new(10).unwrap_or(NonZeroUsize::MIN)" not in audio:
        raise AssertionError("PCM cache capacity must avoid unwrap()")
    if "fn lock_pcm_cache() -> MutexGuard<'static, LruCache<String, (u32, Vec<i16>)>>" not in audio:
        raise AssertionError("audio.rs must centralize PCM cache locking behind lock_pcm_cache()")
    if "Recovering poisoned PCM decode cache" not in audio:
        raise AssertionError("PCM cache must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in audio:
        raise AssertionError("PCM cache must recover poisoned locks with poisoned.into_inner()")
    if "pcm_cache_clear_recovers_poisoned_lock" not in audio:
        raise AssertionError("audio.rs must keep a unit test for poisoned PCM cache clear recovery")
    if "decode_to_pcm_cache_is_bound_to_audio_content_not_path" not in audio:
        raise AssertionError("audio.rs must keep a regression for same-path changed audio cache safety")


def test_vad_cache_recovers_poisoned_lock() -> None:
    audio = (REPO_ROOT / "src-tauri/src/audio.rs").read_text(encoding="utf-8")
    if "fn lock_vad_cache() -> MutexGuard<'static, VadCacheEntry>" not in audio:
        raise AssertionError("audio.rs must centralize VAD cache locking behind lock_vad_cache()")
    if "Recovering poisoned VAD session cache" not in audio:
        raise AssertionError("VAD cache must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in audio:
        raise AssertionError("VAD cache must recover poisoned locks with poisoned.into_inner()")
    if "vad_cache_recovers_poisoned_lock" not in audio:
        raise AssertionError("audio.rs must keep a unit test for poisoned VAD cache recovery")


def test_health_system_recovers_poisoned_lock() -> None:
    health = (REPO_ROOT / "src-tauri/src/health.rs").read_text(encoding="utf-8")
    if "fn lock_system() -> MutexGuard<'static, System>" not in health:
        raise AssertionError("health.rs must centralize system health locking behind lock_system()")
    if "Recovering poisoned system health lock" not in health:
        raise AssertionError("System health locking must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in health:
        raise AssertionError("System health locking must recover poisoned locks with poisoned.into_inner()")
    direct_lock_count = health.count("SYS.lock()")
    if direct_lock_count != 2:
        raise AssertionError(
            f"SYS.lock() must only appear inside lock_system() and poison_system_lock_for_test(), found {direct_lock_count}"
        )
    if "fn poison_system_lock_for_test()" not in health:
        raise AssertionError("health.rs must isolate test-only system lock poisoning behind poison_system_lock_for_test()")
    if "system_health_recovers_poisoned_lock" not in health:
        raise AssertionError("health.rs must keep a unit test for poisoned system health recovery")


def test_pipeline_import_status_recovers_poisoned_lock() -> None:
    pipeline = pipeline_surface()
    if "fn lock_import_status(&self) -> MutexGuard<'_, ImportStatus>" not in pipeline:
        raise AssertionError("ProcessingPipeline must centralize import status locking behind lock_import_status()")
    if "Recovering poisoned import status lock" not in pipeline:
        raise AssertionError("Import status locking must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in pipeline:
        raise AssertionError("Import status locking must recover poisoned locks with poisoned.into_inner()")
    direct_lock_count = pipeline.count("self.import_status.lock()")
    if direct_lock_count != 1:
        raise AssertionError(f"self.import_status.lock() must only appear inside lock_import_status(), found {direct_lock_count}")
    if "import_status_recovers_poisoned_lock" not in pipeline:
        raise AssertionError("pipeline.rs must keep a unit test for poisoned import-status recovery")


def test_pipeline_cached_services_recover_poisoned_locks() -> None:
    pipeline = pipeline_surface()
    if "fn lock_diarization_service(" not in pipeline:
        raise AssertionError("ProcessingPipeline must centralize diarization service locking")
    if "fn lock_denoiser_service(&self) -> MutexGuard<'_, Option<crate::denoiser::DenoiserService>>" not in pipeline:
        raise AssertionError("ProcessingPipeline must centralize denoiser service locking")
    if "Recovering poisoned diarization service lock" not in pipeline:
        raise AssertionError("Diarization service locking must warn when recovering a poisoned lock")
    if "Recovering poisoned denoiser service lock" not in pipeline:
        raise AssertionError("Denoiser service locking must warn when recovering a poisoned lock")
    if pipeline.count("self.diarization_service.lock()") != 1:
        raise AssertionError("self.diarization_service.lock() must only appear inside lock_diarization_service()")
    if pipeline.count("self.denoiser_service.lock()") != 1:
        raise AssertionError("self.denoiser_service.lock() must only appear inside lock_denoiser_service()")
    if "service_locks_recover_poisoned_state" not in pipeline:
        raise AssertionError("pipeline.rs must keep a unit test for poisoned service-lock recovery")


def test_pipeline_decoded_window_accumulator_recovers_poisoned_lock() -> None:
    pipeline = pipeline_surface()
    if "fn lock_decoded_windows(windows: &Mutex<Vec<audio::PcmWindow>>) -> MutexGuard<'_, Vec<audio::PcmWindow>>" not in pipeline:
        raise AssertionError("pipeline.rs must centralize decoded-window accumulator locking")
    if "Recovering poisoned decoded PCM window accumulator" not in pipeline:
        raise AssertionError("Decoded-window accumulator locking must warn when recovering a poisoned lock")
    if "poisoned.into_inner()" not in pipeline:
        raise AssertionError("Decoded-window accumulator locking must recover poisoned locks with poisoned.into_inner()")
    if "lock_decoded_windows(&acc).push(window)" not in pipeline:
        raise AssertionError("Streaming decode callback must push windows through lock_decoded_windows()")
    if "lock_decoded_windows(&windows)" not in pipeline:
        # The outer access to the accumulator must still go through the poison-safe helper. (It now MOVEs
        # the Vec out via std::mem::take instead of cloning it — the safety invariant is the guarded
        # access, not the copy.)
        raise AssertionError("Streaming decode must access accumulated windows through lock_decoded_windows()")
    if "decoded_window_accumulator_recovers_poisoned_lock" not in pipeline:
        raise AssertionError("pipeline.rs must keep a unit test for poisoned decoded-window recovery")


def test_telemetry_tracer_recovers_poisoned_span_buffer() -> None:
    telemetry = (REPO_ROOT / "src-tauri/src/telemetry/mod.rs").read_text(encoding="utf-8")
    if "fn lock_spans(&self) -> MutexGuard<'_, Vec<Span>>" not in telemetry:
        raise AssertionError("Tracer must centralize span locking behind lock_spans()")
    if "Recovering poisoned telemetry span buffer" not in telemetry:
        raise AssertionError("Tracer must warn when recovering a poisoned span buffer")
    if "poisoned.into_inner()" not in telemetry:
        raise AssertionError("Tracer must recover poisoned span locks with poisoned.into_inner()")
    direct_lock_count = telemetry.count("self.spans.lock()")
    if direct_lock_count != 1:
        raise AssertionError(f"self.spans.lock() must only appear inside Tracer::lock_spans(), found {direct_lock_count}")
    if "self.tracer.spans.lock()" in telemetry:
        raise AssertionError("SpanGuard must record through Tracer::lock_spans()")
    if "tracer_recovers_poisoned_span_buffer" not in telemetry:
        raise AssertionError("telemetry/mod.rs must keep a unit test for poisoned span-buffer recovery")


def test_finetuned_fallback_counter_excludes_the_wsl_7b_primary_path() -> None:
    """build_segments_from_pcm's fine-tuned FALLBACK counter (finetuned_fallbacks) drives the completion-time
    "stock-grade" downgrade alarm (finetuned_downgrade_message). A fine-tuned miss counts as a STOCK downgrade
    ONLY when the chunk actually falls back to stock local CTC. When WSL 7B is the primary drafter, a miss
    falls to the 7B champion via the "[Pending WSL 7B ASR]" placeholder — NOT stock — so counting it raises a
    FALSE "ALL N chunk(s) were drafted by the STOCK engine … stock-grade" PipelineEvent::Error on an import the
    7B actually drafted (the owner's WSL7B + use_finetuned config when the fine-tuned checkpoint is absent — a
    direct honesty-law violation). The finetuned_fallbacks.fetch_add must be gated to EXCLUDE the wsl-primary
    path. Exercising it end-to-end needs the full import harness + a live 7B server, so it is source-pinned
    (like finetuned_downgrade_message's own unit test)."""
    pipeline = pipeline_surface()
    anchor = "self.finetuned_fallbacks.fetch_add("
    idx = pipeline.find(anchor)
    if idx == -1:
        raise AssertionError("finetuned_fallbacks.fetch_add not found — this gate would pass vacuously")
    # The condition immediately preceding the increment must carry the wsl-primary exclusion, so a fine-tuned
    # miss while WSL 7B is the primary is NOT miscounted as a stock downgrade.
    guard_window = pipeline[max(0, idx - 240):idx]
    if "!wsl_primary" not in guard_window and "!self.should_use_wsl_primary_asr()" not in guard_window:
        raise AssertionError(
            "finetuned_fallbacks.fetch_add is not gated on the wsl-primary exclusion — a fine-tuned miss while "
            "WSL 7B is the primary drafter is miscounted as a STOCK downgrade, emitting a false 'stock-grade' "
            "completion error on a 7B-drafted import. Guard the increment with `&& !wsl_primary` (the chunk "
            "falls to the 7B champion, not stock local CTC)."
        )


def test_asr_load_gate_verifies_the_tokens_vocab_not_only_the_model() -> None:
    """KurdishAsrService::new_with_config runtime-verifies the model.int8.onnx SHA-256 (M2.3 charter: "a
    tampered ONNX fails the runtime manifest check") but the CTC tokens.txt — the index->grapheme map, equally
    load-bearing for output correctness and equally pinned (OMNIASR_CTC_*_TOKENS carry real SHAs in MODELS) —
    was set straight into the recognizer WITHOUT verification. A tampered/swapped same-line-count tokens.txt
    still builds a recognizer (OfflineRecognizer::create succeeds) but decodes every clip to the WRONG Kurdish
    graphemes, persisted at the fixed heuristic confidence and exported as trustworthy, with no gate flagging
    it. The load gate must verify_model_path_runtime the tokens path too. Exercising it end-to-end needs the
    real ~50MB ONNX (and must not tamper the real install), so it is source-pinned."""
    asr = (REPO_ROOT / "src-tauri" / "src" / "asr.rs").read_text(encoding="utf-8")
    start = asr.find("pub fn new_with_config(")
    if start == -1:
        raise AssertionError("new_with_config not found in asr.rs — this gate would pass vacuously")
    rest = asr[start + 10 :]
    end = len(rest)
    for marker in ("\n    pub fn ", "\n    fn "):
        idx = rest.find(marker)
        if idx != -1:
            end = min(end, idx)
    body = rest[:end]
    if "verify_model_path_runtime" not in body:
        raise AssertionError("new_with_config no longer runtime-verifies any model file — gate vacuous")
    if "OMNIASR_CTC_300M_TOKENS" not in body or "OMNIASR_CTC_1B_TOKENS" not in body:
        raise AssertionError(
            "new_with_config verifies the model weights but NOT tokens.txt — a tampered/swapped tokens vocab "
            "(same line count) builds a recognizer that decodes to the wrong graphemes with no integrity gate. "
            "verify_model_path_runtime the tokens path against OMNIASR_CTC_300M_TOKENS / OMNIASR_CTC_1B_TOKENS too."
        )


def test_snapshot_restore_preserves_live_cloud_consent() -> None:
    """restore_db_from_snapshot copies the SNAPSHOT's settings.json over the live one and applies it to memory
    AND the running pipeline. A snapshot captured while the cloud opt-ins were ON would then silently re-grant
    consent the user has since revoked — subsequent transcribe/refine/jury could transmit audio/transcript to a
    cloud provider with no fresh acknowledgment, violating the hard guardrail 'never send without acknowledged
    consent'. Consent is a LIVE per-session privacy decision, not dataset state a rollback should change, so the
    restore must carry the CURRENT opt-ins across (a restore may narrow consent, never escalate it). Needs
    AppState + DB + files, so source-pinned."""
    surface = command_surface()
    start = surface.find("fn restore_db_from_snapshot(")
    if start == -1:
        raise AssertionError("restore_db_from_snapshot not found — this gate would pass vacuously")
    rest = surface[start:]
    end = len(rest)
    for marker in ("\n#[tauri::command]", "\npub async fn ", "\npub fn ", "\nasync fn ", "\nfn "):
        idx = rest.find(marker, 1)
        if idx != -1:
            end = min(end, idx)
    body = rest[:end]
    # Strip comment-only lines so a commented-out call can't satisfy the substring checks below (a
    # commented `// restored.save(...)` must NOT count as persisting).
    code_body = "\n".join(ln for ln in body.splitlines() if not ln.lstrip().startswith("//"))
    for flag in ("cloud_llm_opt_in", "cloud_stt_opt_in", "jury_cloud_opt_in"):
        if flag not in code_body:
            raise AssertionError(
                f"restore_db_from_snapshot does not preserve the live consent flag {flag} — restoring a "
                "cloud-ON-era snapshot silently re-enables cloud consent the user revoked. Carry the current "
                "state.lock_settings() opt-ins across the restore instead of adopting the snapshot's."
            )
    # The in-memory narrowing above is NOT enough: the fs::copy of the snapshot's settings.json already
    # overwrote the on-disk file with the snapshot's (possibly cloud-ON) opt-ins, and AppSettings::load does
    # not reset opt-ins — so without persisting the narrowed struct back to disk, the NEXT launch silently
    # re-grants the revoked consent. The restore MUST save the narrowed settings to disk (hunt-3 / iter 157).
    if "restored.save(" not in code_body:
        raise AssertionError(
            "restore_db_from_snapshot narrows consent IN MEMORY but never persists the narrowed settings to "
            "disk. The snapshot's settings.json was already copied over the live one, so the next launch's "
            "AppSettings::load re-grants the revoked cloud consent. Call restored.save(&data_dir.join("
            "\"settings.json\")) after narrowing the opt-ins."
        )
    # The persist-back alone is not enough: the EXTRA_STATE copy loop must SKIP settings.json, so the
    # snapshot's (possibly cloud-ON) opt-ins never touch data_dir/settings.json in the first place. Otherwise a
    # plain fs::copy writes the cloud-ON value and, if the best-effort re-save then fails/is-interrupted (disk
    # full during recovery, or a process kill in the window), the revoked consent silently returns on the next
    # launch. The narrowed struct must be the FIRST and ONLY settings.json write (hunt-9 / iter 165).
    if 'if extra == "settings.json"' not in code_body:
        raise AssertionError(
            "restore_db_from_snapshot's EXTRA_STATE loop plain-copies settings.json — the snapshot's cloud "
            "opt-ins reach disk before the consent-narrowing re-save, so a failed/interrupted re-save re-grants "
            "revoked consent on the next launch. Skip settings.json in the copy loop (`if extra == "
            "\"settings.json\" { continue; }`) and load the snapshot's settings for narrowing instead."
        )


def test_bundle_runconfig_denoising_reflects_loadability_not_mere_presence() -> None:
    """The bundle manifest's runConfig.denoising is the provenance record of whether audio was denoised.
    config_from_settings(settings, ACTIVE) computes `enable_denoising && ACTIVE`, and DenoiserService::is_active's
    contract explicitly forbids recording denoising the model did not perform. denoiser_present() is a mere disk
    check (GTCRN file exists >=400KB), so a present-but-unloadable model (opset/EP incompatibility, provider init
    failure) would record denoising=true while the pipeline correctly left the audio un-denoised (pipeline.rs:1780
    warns). The bundle must pass the LOADABILITY signal (denoiser_loadable / is_active), not denoiser_present.
    Exercising it needs the real GTCRN ONNX to actually load, so it is source-pinned."""
    bundle = (REPO_ROOT / "src-tauri" / "src" / "export_bundle.rs").read_text(encoding="utf-8")
    if "config_from_settings(settings, model_manager.denoiser_present())" in bundle:
        raise AssertionError(
            "bundle runConfig passes denoiser_present() (mere disk presence) to config_from_settings — records "
            "denoising=true for a present-but-unloadable model, a provenance lie. Pass model_manager."
            "denoiser_loadable() (actual is_active) instead."
        )
    if "denoiser_loadable" not in bundle:
        raise AssertionError(
            "bundle runConfig does not use the denoiser LOADABILITY signal — denoising provenance may over-claim."
        )


def test_default_transcribe_segment_refuses_blank_draft() -> None:
    """The DEFAULT `transcribe_segment` command must refuse a blank draft (return Err before the Ok
    json), exactly as its two opt-in siblings transcribe_segment_constrained/_finetuned already do.
    Otherwise App.svelte's handleTranscribe upserts "" over an existing good transcript
    (annotatedTranscript = result.text, then updateSegment) — silent, irreversible data loss (the
    recurring blank-transcript-never-overwrites-good class). Exercising it needs a real pipeline + models
    (WSL/ONNX), so it is source-pinned. `transcribe_segment(` matches only the default (the siblings are
    `transcribe_segment_constrained(` / `_finetuned(`)."""
    src = (COMMANDS_DIR / "transcribe.rs").read_text(encoding="utf-8")
    start = src.find("pub async fn transcribe_segment(")
    if start == -1:
        raise AssertionError("default transcribe_segment command not found in commands/transcribe.rs")
    # The command body runs up to the NEXT #[tauri::command] (align_segment), which bounds it.
    end = src.find("#[tauri::command]", start)
    body = src[start:end] if end != -1 else src[start:]
    if ".trim().is_empty()" not in body or "return Err" not in body:
        raise AssertionError(
            "default transcribe_segment does not guard a blank draft — an empty ASR result would upsert "
            '"" over a good transcript (data loss). Return Err when draft.final_text.trim().is_empty(), '
            "matching transcribe_segment_constrained/_finetuned."
        )


def test_batch_transcribe_refuses_blank_draft() -> None:
    """batch_transcribe must SKIP a blank draft, not persist it. update_batch_transcription_if_unreviewed
    (db.rs) writes raw_transcript unconditionally, guarding only human-reviewed rows — so an empty ASR
    result would overwrite an existing good UNREVIEWED transcript with "" (e.g. a jury_accept 7B draft
    re-batch-transcribed by the offline CTC engine that returns Ok("") on a quiet clip). Recurring
    blank-transcript-never-overwrites-good data-loss class. The runtime path needs a full app + pipeline,
    so it is source-pinned."""
    src = (REPO_ROOT / "src-tauri" / "src" / "commands.rs").read_text(encoding="utf-8")
    if "update_batch_transcription_if_unreviewed" not in src:
        raise AssertionError("batch_transcribe persist call not found in commands.rs")
    guard = "Ok(draft) if draft.final_text.trim().is_empty() && draft.raw_text.trim().is_empty()"
    if guard not in src:
        raise AssertionError(
            "batch_transcribe does not guard a blank draft before update_batch_transcription_if_unreviewed "
            '— an empty ASR result would overwrite a good unreviewed transcript with "". Add the match-guard '
            "arm that skips when final_text and raw_text are both empty."
        )


def test_batch_processor_never_deletes_a_segment_with_an_existing_transcript() -> None:
    """The headless batch_processor prunes fresh placeholder segments (VAD false-positives), but must
    NEVER delete a segment that already holds a real transcript just because THIS run's bundled-engine
    re-transcription is empty/silent/unsliceable — that destroys a good draft (e.g. a stronger WSL-7B
    draft), silent data loss. Every `to_delete.push` must be guarded by `has_existing_transcript`. The
    runtime path needs real ONNX models + a DB, so it is source-pinned."""
    src = (REPO_ROOT / "src-tauri" / "src" / "bin" / "batch_processor.rs").read_text(encoding="utf-8")
    if "has_existing_transcript" not in src:
        raise AssertionError(
            "batch_processor computes no has_existing_transcript guard — an empty/silent re-transcription "
            "can delete a segment that already holds a good draft."
        )
    deletes = src.count("to_delete.push(seg.id.clone())")
    guards = src.count("if !has_existing_transcript {")
    if deletes == 0 or guards < deletes:
        raise AssertionError(
            f"batch_processor has {deletes} to_delete.push site(s) but only {guards} has_existing_transcript "
            "guard(s) — an unguarded delete can destroy a segment that already has a good transcript."
        )


def test_constrained_transcribe_verifies_model_and_tokens_pin() -> None:
    """The opt-in constrained decode loads model.int8.onnx + tokens.txt via ort with NO built-in SHA check
    (constrained_decode::run_constrained), so its command must enforce the SAME runtime integrity pin the
    default ASR path does (asr.rs:288-311): a tampered/swapped same-line-count tokens.txt still loads but
    decodes every clip to the WRONG graphemes, persisted as trustworthy. Runtime needs the real ONNX, so
    it is source-pinned. `transcribe_segment_constrained(` matches only the constrained command."""
    src = (COMMANDS_DIR / "transcribe.rs").read_text(encoding="utf-8")
    start = src.find("pub async fn transcribe_segment_constrained(")
    if start == -1:
        raise AssertionError("transcribe_segment_constrained command not found in commands/transcribe.rs")
    end = src.find("#[tauri::command]", start)
    body = src[start:end] if end != -1 else src[start:]
    if body.count("verify_model_path_runtime(") < 2:  # call form only (a comment mention has no '(')
        raise AssertionError(
            "constrained transcribe command does not verify BOTH the model and tokens SHA-256 pin before "
            "decoding — a tampered/swapped tokens.txt would decode to the wrong graphemes, persisted as "
            "trustworthy. Add verify_model_path_runtime for the model AND the tokens (mirror asr.rs)."
        )


def test_check_audio_and_get_duration_share_the_decode_fallback() -> None:
    """check_audio_file and get_duration_ms must compute duration through the SAME helper
    (duration_ms_with_decode_fallback). They previously duplicated the logic and diverged: check_audio_file
    used num_frames.unwrap_or(0) with NO fallback and reported 0 ms for a valid VBR MP3 (no Xing/Info
    header), contradicting get_duration_ms's true (decode-based) value. Exercising the no-frame-count
    fallback needs a real decode (flaky to unit-test under the parallel suite), so the consolidation is
    source-pinned here."""
    src = (REPO_ROOT / "src-tauri" / "src" / "audio.rs").read_text(encoding="utf-8")
    if "duration_ms: duration_ms_with_decode_fallback(" not in src:
        raise AssertionError(
            "check_audio_file does not compute duration_ms via the shared duration_ms_with_decode_fallback "
            "helper — it would report 0 ms for a valid file with no frame count (VBR MP3 without a Xing "
            "header), diverging from get_duration_ms."
        )
    if "duration_ms_with_decode_fallback(path.as_ref()" not in src:
        raise AssertionError(
            "get_duration_ms must also route through duration_ms_with_decode_fallback so the two functions "
            "can never disagree on a no-frame-count file."
        )


def test_bundled_only_models_resolve_per_file_not_all_or_nothing() -> None:
    """The denoiser/aligner/campp speaker model live ONLY in the bundled (exe-adjacent) dir on a fresh
    install. Loading them via `resolved_dir()` — which is all-or-nothing: any OmniASR in the user dir flips
    the whole root — orphans them once the user downloads an OmniASR model (silent VAD→energy /
    no-denoise / no-diarization degradation). They must resolve PER FILE via `resolve_root_for`. Recurring
    class (VAD, denoiser, aligner+campp). Runtime path needs real models, so source-pinned."""
    src = (REPO_ROOT / "src-tauri" / "src" / "pipeline.rs").read_text(encoding="utf-8")
    for bad in (
        "SpeakerEmbeddingService::new(&self.model_manager.resolved_dir()",
        "ForcedAligner::new(&self.model_manager.resolved_dir()",
    ):
        if bad in src:
            raise AssertionError(
                f"a bundled-only model is loaded via all-or-nothing resolved_dir() ({bad}) — it will be "
                "orphaned when the user downloads OmniASR. Use model_manager.resolve_root_for(<file>)."
            )
    if "resolve_root_for(crate::models::CAMPP_MODEL)" not in src or 'resolve_root_for("mms_aligner.onnx")' not in src:
        raise AssertionError(
            "the campp speaker model and mms_aligner must resolve per-file via resolve_root_for."
        )


def test_wsl_refinement_loop_refuses_blank_draft() -> None:
    """The WSL-7B batch refinement loop persists via update_asr_transcript_if_unreviewed, which writes
    raw_transcript unconditionally (guarding only human-reviewed rows) — so a blank 7B result (Ok("") for a
    silent/music/noise clip, per parse_wsl_segment_result) would overwrite a good existing transcript. It
    must SKIP a blank draft. Runtime path needs the WSL server, so source-pinned. (blank-transcript-never-
    overwrites-good class; siblings transcribe_segment / batch_transcribe / batch_processor.)"""
    src = (REPO_ROOT / "src-tauri" / "src" / "commands.rs").read_text(encoding="utf-8")
    if "update_asr_transcript_if_unreviewed" not in src:
        raise AssertionError("WSL refinement persist call not found in commands.rs")
    if "Ok((raw_transcript, _)) if raw_transcript.trim().is_empty()" not in src:
        raise AssertionError(
            "the WSL-7B refinement loop does not guard a blank draft before update_asr_transcript_if_unreviewed "
            '— an empty 7B result would overwrite a good transcript with "". Add the guarded match arm.'
        )


def test_writers_active_includes_the_wsl_refinement_writer() -> None:
    """prepare_restore refuses a DB restore only while writers_active(). The WSL-7B refinement loop is a
    background DB WRITER (update_asr_transcript_if_unreviewed) tracked by WSL_REFINE_RUNNING, so
    writers_active() must include it — else a restore runs mid-refinement-write and tears the DB. Runtime
    concurrency isn't unit-testable without a live batch, so source-pinned."""
    src = (REPO_ROOT / "src-tauri" / "src" / "lib.rs").read_text(encoding="utf-8")
    start = src.find("pub fn writers_active(&self) -> bool {")
    if start == -1:
        raise AssertionError("writers_active not found in lib.rs")
    body = src[start : src.find("}", start)]
    if "WSL_REFINE_RUNNING" not in body:
        raise AssertionError(
            "writers_active() does not account for the WSL-7B refinement writer (WSL_REFINE_RUNNING) — a DB "
            "restore could run mid-refinement-write and tear the DB. Include it alongside import/batch state."
        )


def _async_fn_body(source: str, fn_name: str) -> str:
    """Text from `pub async fn <fn_name>` up to the next `pub async fn` (or EOF) — a cheap way to
    scope a per-command content check to one command's body without a full Rust parse."""
    marker = f"pub async fn {fn_name}"
    start = source.find(marker)
    if start == -1:
        raise AssertionError(f"{fn_name} not found in export.rs — this gate would pass vacuously")
    rest = source[start + len(marker):]
    nxt = rest.find("pub async fn ")
    return rest if nxt == -1 else rest[:nxt]


def test_directory_exports_validate_out_dir_against_unc() -> None:
    # hunt-10 #1: export_gold_eval_set / export_finetune_pack took out_dir with ONLY a null-byte check,
    # skipping validate::validate_output_path — the UNC/null trust-boundary guard every sibling export
    # (incl. the directory export export_audio) enforces. A compromised/XSS'd renderer could hand a
    # `\\attacker\share` out_dir straight to eval::create_dir_all(out_dir.join("clips")), driving the
    # Windows SMB redirector (forced-auth NTLM-credential leak) and writing the gold clips off-machine.
    # A bare out_dir.contains('\0') is NOT sufficient; the syntactic UNC guard is the actual defense.
    export_rs = (COMMANDS_DIR / "export.rs").read_text(encoding="utf-8")
    for fn in ("export_gold_eval_set", "export_finetune_pack"):
        body = _async_fn_body(export_rs, fn)
        if "validate::validate_output_path" not in body:
            raise AssertionError(
                f"{fn} must route out_dir through validate::validate_output_path (UNC/null guard) "
                f"before it reaches create_dir_all — a null-byte-only check leaves the SMB/NTLM leak open."
            )


def main() -> None:
    test_directory_exports_validate_out_dir_against_unc()
    test_known_runtime_panic_patterns_do_not_return()
    test_wsl_refinement_batch_is_panic_safe_and_cancellable()
    test_wsl_refinement_lifecycle_failures_are_reported()
    test_app_entrypoint_reports_fatal_errors_without_panicking()
    test_app_state_cancel_token_recovers_poisoned_lock()
    test_commands_use_recovered_app_state_cancellation_api()
    test_app_state_import_state_recovers_poisoned_lock()
    test_commands_use_recovered_app_state_import_api()
    test_app_state_batch_state_recovers_poisoned_lock()
    test_commands_use_recovered_app_state_batch_api()
    test_app_state_pipeline_settings_update_recovers_poisoned_lock()
    test_session_cleanup_reports_failures()
    test_instance_lock_cleanup_reports_failures()
    test_commands_do_not_silently_default_critical_db_failures()
    test_commands_batch_transcribe_reports_insert_failures()
    test_commands_jury_evidence_serialization_is_not_silent()
    test_jury_background_runs_report_failures()
    test_pipeline_event_emits_report_failures()
    test_command_event_emits_are_not_silently_discarded()
    test_commands_audio_duration_probe_send_failures_are_reported()
    test_commands_batch_normalize_reports_prefetch_and_update_failures()
    test_commands_acoustic_scoring_reports_skipped_segments()
    test_alignment_json_and_quality_are_written_as_one_atomic_statement()
    test_media_cache_cleanup_reports_failures()
    test_jury_db_and_export_paths_do_not_silently_drop_errors()
    test_database_read_paths_do_not_silently_drop_rows()
    test_database_fts_maintenance_does_not_silently_discard_errors()
    test_database_savepoint_cleanup_reports_failures()
    test_atomic_file_cleanup_reports_failures()
    test_model_artifact_cleanup_reports_failures()
    test_model_metadata_updates_do_not_silently_default()
    test_audio_decode_worker_send_failures_are_reported()
    test_audio_decode_reset_required_fails_loud_not_silent_truncation()
    test_pipeline_wsl_subprocess_send_failures_are_reported()
    test_wsl_import_pass_does_not_roll_back_the_file_on_a_benign_empty_result()
    test_probe_wsl_7b_server_reaps_child_on_wait_error()
    test_aligner_score_consistency_caps_clip_and_dp()
    test_finish_import_clears_cancel_token_before_opening_the_gate()
    test_batch_normalize_uses_a_targeted_update_not_a_whole_row_upsert()
    test_save_session_is_rate_limited()
    test_atomic_replace_post_swap_backup_cleanup_is_best_effort()
    test_t0_calibration_excludes_human_rejected_from_the_conformal_set()
    test_run_t2_for_segment_respects_the_autonomy_dial()
    test_optin_transcribe_commands_reject_a_blank_decode()
    test_pipeline_wsl_retranscribe_rejects_an_empty_result()
    test_finetuned_fallback_counter_excludes_the_wsl_7b_primary_path()
    test_asr_load_gate_verifies_the_tokens_vocab_not_only_the_model()
    test_snapshot_restore_preserves_live_cloud_consent()
    test_bundle_runconfig_denoising_reflects_loadability_not_mere_presence()
    test_pipeline_duration_probe_failures_are_not_silent()
    test_export_bundle_model_metadata_load_errors_are_visible()
    test_eval_read_paths_do_not_silently_drop_rows()
    test_pipeline_rediarize_reports_db_update_failures()
    test_batch_processor_asr_errors_are_not_blank_transcripts()
    test_default_transcribe_segment_refuses_blank_draft()
    test_batch_transcribe_refuses_blank_draft()
    test_batch_processor_never_deletes_a_segment_with_an_existing_transcript()
    test_constrained_transcribe_verifies_model_and_tokens_pin()
    test_check_audio_and_get_duration_share_the_decode_fallback()
    test_bundled_only_models_resolve_per_file_not_all_or_nothing()
    test_wsl_refinement_loop_refuses_blank_draft()
    test_writers_active_includes_the_wsl_refinement_writer()
    test_pipeline_hypothesis_population_reports_failures()
    test_asr_pool_recovers_poisoned_state_lock()
    test_global_rate_limiter_recovers_poisoned_lock()
    test_audio_fingerprint_cache_recovers_poisoned_lock()
    test_transcript_cache_recovers_poisoned_lock_and_never_zero_capacity()
    test_memoizer_recovers_poisoned_lock_and_never_zero_capacity()
    test_history_manager_recovers_poisoned_stacks()
    test_inference_metrics_recover_poisoned_locks()
    test_pcm_cache_recovers_poisoned_lock()
    test_vad_cache_recovers_poisoned_lock()
    test_health_system_recovers_poisoned_lock()
    test_pipeline_import_status_recovers_poisoned_lock()
    test_pipeline_cached_services_recover_poisoned_locks()
    test_pipeline_decoded_window_accumulator_recovers_poisoned_lock()
    test_telemetry_tracer_recovers_poisoned_span_buffer()
    print("rust runtime panic policy regression passed")


if __name__ == "__main__":
    main()
