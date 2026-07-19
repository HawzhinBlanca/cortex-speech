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
        'tracing::error!("Batch normalize DB lookup failed before update for {id}: {error}")',
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
    ]
    present = [pattern for pattern in forbidden if pattern in db]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"db.rs silently discards FTS maintenance errors:\n{formatted}")

    required = [
        'self.conn.execute_batch("INSERT INTO segments_fts(segments_fts) VALUES(\'rebuild\');")?;',
        'tracing::warn!("Failed to optimize segments FTS index after batch delete: {error}");',
        "fn fts_index_searches_inserted_segments_and_tracks_batch_delete()",
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


def main() -> None:
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
    test_pipeline_wsl_subprocess_send_failures_are_reported()
    test_pipeline_duration_probe_failures_are_not_silent()
    test_export_bundle_model_metadata_load_errors_are_visible()
    test_eval_read_paths_do_not_silently_drop_rows()
    test_pipeline_rediarize_reports_db_update_failures()
    test_batch_processor_asr_errors_are_not_blank_transcripts()
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
