"""Durable restore-generation admission policy.

Pins the state machine, cancellation-safe cleanup, startup recovery ordering, long-operation mutation
guards, and cross-process writer locks. This is a deterministic source scan: it does not mutate the
global admission state and cannot flake concurrent Rust tests.
"""

from pathlib import Path

from _command_policy_util import command_production_surface, command_surface
from _couch_policy_util import couch_surface
from _db_policy_util import database_surface

REPO_ROOT = Path(__file__).resolve().parents[1]
SRC = REPO_ROOT / "src-tauri" / "src"


def _read(rel: str) -> str:
    if rel == "commands.rs":
        return command_surface(SRC)
    if rel == "couch.rs":
        return couch_surface(SRC)
    if rel == "db.rs":
        return database_surface(SRC)
    return (SRC / rel).read_text(encoding="utf-8")


def _fn_body(src: str, signature_start: str, span: int = 1400) -> str:
    idx = src.find(signature_start)
    if idx == -1:
        raise AssertionError(f"could not find `{signature_start}` — restore-reservation gate cannot verify it")
    return src[idx : idx + span]


def test_prepare_restore_reserves_before_the_fence_and_returns_the_guard() -> None:
    commands = _read("commands.rs")
    recovery_ipc = _read("commands/recovery_ipc.rs")
    adapter = _fn_body(recovery_ipc, "fn prepare_restore(", span=1200)
    if "prepare_restore_admission(data_dir, || state.writers_active())" not in adapter:
        raise AssertionError("the Tauri command adapter must delegate admission and its writer fence")
    recovery = _read("recovery.rs")
    if "use tauri" in recovery or "crate::AppState" in recovery or "crate::commands" in recovery:
        raise AssertionError("restore admission/marker authority must remain Tauri- and command-free")
    public = _fn_body(recovery, "pub(crate) fn prepare_restore_admission(", span=700)
    if "RestoreReservation<'static>" not in public or "prepare_restore_admission_with(" not in public:
        raise AssertionError("recovery admission must return a RestoreReservation held across restore")
    body = _fn_body(recovery, "fn prepare_restore_admission_with<'a>(", span=1800)
    durable = body.find("named_restore_barrier_may_exist(&data_dir)")
    recover = body.find("admission.claim_recovery()")
    reserve = body.find("admission.try_reserve()")
    fence = body.find("writers_active()")
    if -1 in (durable, recover, reserve, fence) or not (durable < recover < fence and reserve < fence):
        raise AssertionError(
            "recovery admission must reclaim a durable marker or reserve a new generation BEFORE "
            "checking writers_active(), so a racing writer cannot cross the boundary"
        )
    for moved in (
        "fn load_named_restore_pending(",
        "fn named_restore_barrier_may_exist(",
        "fn write_named_restore_pending(",
        "fn mark_named_restore_completed(",
        "fn clear_review_pilot_restore_pending(",
        "fn preserve_live_asr_runtime_controls(",
        "fn restore_required_snapshot_state_atomic(",
        "fn apply_snapshot_pilot_policy(",
        "fn strict_live_settings_for_restore(",
        "fn install_snapshot_restore_plan(",
        "fn inspect_snapshot_pilot_policy(",
        "fn explicit_snapshot_pilot_policy(",
        "fn inspect_snapshot_restore_plan(",
        "fn prepare_named_restore_artifacts",
        "fn take_mandatory_pre_restore_snapshot(",
        "fn pin_selector(",
        "fn begin_named_restore_transaction(",
    ):
        if moved in commands or moved not in recovery:
            raise AssertionError(f"durable restore marker authority was not isolated: {moved}")


def test_restore_service_owns_sql_authority_without_ui_dependencies() -> None:
    service_dir = SRC / "restore_service"
    service_files = sorted(service_dir.glob("*.rs"))
    if not service_files:
        raise AssertionError("restore service is missing — command-layer SQL isolation cannot be verified")
    service = "\n".join(path.read_text(encoding="utf-8") for path in service_files)
    for forbidden in ("use tauri", "tauri::", "crate::AppState", "crate::commands"):
        if forbidden in service:
            raise AssertionError(f"restore service depends on the UI/command layer: {forbidden}")
    for required in (
        "fn require_restore_authority_superset(",
        "fn validate_restore_target_semantics(",
        "fn require_active_pilot_policy_binding(",
        "fn restore_with_mandatory_snapshot(",
        "fn prepare_and_restore_named_transaction(",
        "fn recover_interrupted_named_restore_with_admission(",
    ):
        if required not in service:
            raise AssertionError(f"restore service lost authority/orchestration seam: {required}")

    database = _read("db.rs")
    policy4_wrapper = _fn_body(database, "pub(crate) fn validate_policy4_restore_authority(", span=500)
    if "validate_policy4_effect_authority(&self.conn)" not in policy4_wrapper:
        raise AssertionError("restore must reuse the canonical policy-4 semantic validator, not duplicate or skip it")
    playback = _read("restore_service/playback.rs")
    if "db.validate_policy4_restore_authority()" not in playback:
        raise AssertionError("restore playback validation does not prove policy-4 multi-table authority")
    compensation = _read("restore_service/compensation.rs")
    for required in (
        "crate::db::DESKTOP_PLAYBACK_POLICY_VERSION",
        "playback_authority_consumptions_v4",
        "effect.playback_authority_session_id = receipt.authority_session_id",
    ):
        if required not in compensation:
            raise AssertionError(f"restore compensation validation lost policy-4 operation binding: {required}")

    commands = _read("commands.rs")
    for moved in (
        "fn require_restore_authority_superset(",
        "fn validate_restore_target_semantics(",
        "fn require_active_pilot_policy_binding(",
        "fn restore_with_mandatory_snapshot(",
        "fn prepare_and_restore_named_transaction(",
        "fn recover_interrupted_named_restore_with_admission(",
    ):
        if moved in commands:
            raise AssertionError(f"restore authority leaked back into the Tauri command module: {moved}")
    fixture = _fn_body(commands, "fn canonical_policy4_phone_playback(", span=5000)
    for required in (
        "finalize_couch_playback_attempt_v1(",
        "couch_playback_proof_v4(",
        "current_canonical_pcm_blake3(&source_path)",
    ):
        if required not in fixture:
            raise AssertionError(f"restore policy-4 characterization uses a synthetic/bypass fixture: {required}")
    # Recovery commands now own a dedicated module. Inspect that exact boundary instead of slicing
    # the composed command surface between symbols from different files: module concatenation order
    # can otherwise pull unrelated diagnostics code into this SQL scan and create a false verdict.
    adapter = _read("commands/recovery_ipc.rs")
    for forbidden in (".connection()", "rusqlite::", "query_row(", "prepare("):
        if forbidden in adapter:
            raise AssertionError(f"restore command adapter issues SQL instead of calling the service: {forbidden}")


def test_both_restore_callers_hold_the_reservation() -> None:
    commands = _read("commands/recovery_ipc.rs")
    binding = "let (restore_reservation,"
    if commands.count(binding) != 2:
        raise AssertionError(
            "db_restore and restore_db_from_snapshot must each bind the RestoreReservation so the admission "
            "gate stays closed through DB, history, settings, policy, and pipeline completion."
        )
    bare = _fn_body(commands, "pub async fn db_restore(", span=3500)
    history_handle = bare.find("state.history_arc_for_restore()")
    worker = bare.find("run_blocking(move ||")
    publish = bare.find("restore_with_mandatory_snapshot(")
    clear = bare.find("history.lock().unwrap_or_else")
    return_guard = bare.find("Ok(restore_reservation)")
    if -1 in (history_handle, worker, publish, clear, return_guard) or not (
        history_handle < worker < publish < clear < return_guard
    ):
        raise AssertionError(
            "bare restore must clear old-generation history inside the detachable blocking worker before "
            "its reservation can leave or drop"
        )
    if "state.lock_history().clear()" in bare:
        raise AssertionError("bare restore cleanup regressed to post-await code that cancellation can skip")
    lib = _read("lib.rs")
    if "type HistKeyMgr = Arc<Mutex<HistoryManager>>;" not in lib or "history_arc_for_restore" not in lib:
        raise AssertionError("restore worker needs a clonable history handle for cancellation-safe cleanup")


def test_restore_admission_is_exclusive_and_all_appstate_handles_delegate() -> None:
    commands = _read("commands.rs")
    runtime = _read("database_runtime.rs")
    for phase in ("Idle", "ActiveNew", "ActiveArmed", "Parked"):
        if phase not in _fn_body(runtime, "enum RestorePhase", span=700):
            raise AssertionError(f"restore state machine lost phase {phase}")
    reserve = _fn_body(runtime, "fn reserve(", span=4300)
    for needle in (".compare_exchange(", "admission.generation", "RestorePhase::Parked if recovery_required"):
        if needle not in reserve:
            raise AssertionError(f"restore reservation lost ownership/recovery primitive: {needle}")
    drop = _fn_body(runtime, "impl Drop for RestoreReservation", span=1700)
    if "RestorePhase::ActiveArmed" not in drop or "state.phase = RestorePhase::Parked" not in drop:
        raise AssertionError("dropping an armed named restore must park admission, never reopen writers")
    commit = _fn_body(runtime, "fn commit_named_restore", span=1200)
    for needle in ("state.phase = RestorePhase::Idle", "pending.store(false", "complete.notify_all()"):
        if needle not in commit:
            raise AssertionError(f"coherent restore commit lost release step: {needle}")
    for regression in (
        "armed_restore_parks_on_error_and_exact_recovery_is_the_only_reentry",
        "full_operation_mutation_and_restore_admission_are_race_closed",
    ):
        if regression not in commands:
            raise AssertionError(f"missing deterministic restore-admission regression: {regression}")
    if "while self.is_pending()" not in _fn_body(runtime, "fn lock<'a, T>(", span=1400):
        raise AssertionError("ordinary AppState DB locks must wait behind the restore admission barrier")

    lib = _read("lib.rs")
    if "mod database_runtime;" not in lib or "db: DatabaseRuntime" not in lib:
        raise AssertionError("AppState must delegate process-level database ownership to DatabaseRuntime")
    if "self.db.lock()" not in _fn_body(lib, "pub(crate) fn lock_db(", span=500):
        raise AssertionError("AppState::lock_db bypasses the restore admission barrier")
    handle = _fn_body(lib, "impl AppDatabaseHandle", span=700)
    if "self.inner.lock()" not in handle:
        raise AssertionError("the clonable AppState DB handle bypasses the restore admission barrier")
    for needle in ("writer: Arc<Mutex<Database>>", "reads: Arc<ReadConnectionPool>", "admission: Arc<RestoreAdmission>"):
        if needle not in runtime:
            raise AssertionError(f"DatabaseRuntime lost process-level ownership boundary: {needle}")


def test_snapshot_and_restore_share_one_mutex_guard_in_both_commands() -> None:
    commands = _read("commands.rs")
    service = _read("restore_service/orchestration.rs")
    runtime = _read("database_runtime.rs")
    production = command_production_surface(SRC)
    # Restore validation now includes the complete durable-history and semantic gates before the
    # pin. Keep the scan wide enough to include the final publish call as that safety work grows.
    helper = _fn_body(service, "pub(crate) fn restore_with_mandatory_snapshot(", span=1800)
    # Match the production call exactly.  The restore helper uses the fully-qualified path so this
    # gate cannot silently pass against an unrelated `Database` import or a similarly named helper.
    stage = helper.find("crate::db::Database::stage_restore_source(source)")
    snapshot = helper.find("take_mandatory_pre_restore_snapshot(reservation, db, data_dir)?")
    restore = helper.find("db.commit_staged_restore(&staged)")
    if -1 in (stage, snapshot, restore) or not (stage < snapshot < restore):
        raise AssertionError("bare restore must stage/validate first, then pin the live DB, then atomically publish")
    named = _fn_body(service, "pub(crate) fn prepare_and_restore_named_transaction(", span=5200)
    if "prepare_named_restore_artifacts(" not in named or "begin_named_restore_transaction(" not in named:
        raise AssertionError("named restore must bind verified artifacts to its reusable safety-pin transaction")
    if "db.commit_staged_restore(&staged)" not in named:
        raise AssertionError("named restore no longer publishes only the isolated, verified staged database")
    if production.count("restore_with_mandatory_snapshot(&restore_reservation, writer") != 1:
        raise AssertionError("db_restore must call the one-guard bare snapshot+restore helper exactly once")
    if production.count("prepare_and_restore_named_transaction(") != 1:
        raise AssertionError("restore_db_from_snapshot must call the named one-guard transaction helper")
    if "db_arc_for_restore" in production or "writer_arc_for_restore" in runtime:
        raise AssertionError("restore commands must not escape DatabaseRuntime through a raw writer Arc")
    restore_boundary = _fn_body(runtime, "pub(crate) fn with_restore_writer", span=3100)
    for required in (
        "std::ptr::eq(self.admission.as_ref(), reservation.admission)",
        "let reopened = Database::open(self.database_path.as_ref())",
        "let value = operation(&mut writer)?;",
        "*writer = reopened;",
    ):
        if required not in restore_boundary:
            raise AssertionError(f"runtime-owned restore/reopen boundary lost invariant: {required}")
    if restore_boundary.find("let reopened =") > restore_boundary.find("operation(&mut writer)"):
        raise AssertionError("post-restore connection creation must be proven before live-page publication")
    if production.count("database.with_restore_writer(&restore_reservation") != 2:
        raise AssertionError("both restore commands must publish and reopen through DatabaseRuntime")
    if "successful_restore_reopens_the_writer_before_admission_releases" not in runtime:
        raise AssertionError("restore/reopen generation behavior needs a deterministic runtime regression")
    db = _read("db.rs")
    stage = _fn_body(db, "pub(crate) fn stage_restore_source_with_original_evidence", span=5200)
    if "capture_private_restore_source" not in stage or "open_detached_read_snapshot" not in stage:
        raise AssertionError(
            "restore preflight must privately capture main+WAL authority before opening SQLite, so it neither "
            "ignores committed WAL rows nor creates sidecars beside a manifest-bound source"
        )
    if "open_immutable_connection" in stage:
        raise AssertionError("restore preflight regressed to immutable main-file-only authority and can ignore WAL truth")
    if "staged_restore_includes_a_small_committed_wal_generation" not in db:
        raise AssertionError("WAL-aware restore-source admission needs a committed-small-WAL regression")


def test_every_production_database_entrypoint_requires_the_shared_pre_migration_pin() -> None:
    helper = _fn_body(_read("snapshot.rs"), "pub fn initialize_with_required_pre_migration_pin(", span=1800)
    pin = helper.find("take_pinned_snapshot(")
    initialize = helper.find("db.initialize()?")
    if pin == -1 or initialize == -1 or pin >= initialize:
        raise AssertionError("the central initialization guard must promote the pre-migration pin before initialize")
    if "current > 0 && current < max_known" not in helper:
        raise AssertionError("the central initialization guard no longer distinguishes established pending schemas")

    lib = _read("lib.rs")
    desktop = _fn_body(lib, "let db_path = data_dir.join(\"cortex-speech.db\");", span=3000)
    shared_call = "initialize_with_required_pre_migration_pin(&db, &data_dir)"
    if shared_call not in desktop or "db.initialize()" in desktop:
        raise AssertionError("desktop startup must initialize only through the fail-closed pre-migration pin guard")
    if "pre-migration snapshot failed (continuing)" in desktop:
        raise AssertionError("desktop startup has regressed to warn-and-continue after a failed migration safety pin")

    importer = _read("bin/batch_importer.rs")
    if "initialize_with_required_pre_migration_pin(&db, &app_data_dir)" not in importer:
        raise AssertionError("batch_importer bypasses the shared pre-migration safety pin")
    if "db.initialize()" in importer:
        raise AssertionError("batch_importer still has a direct migration path that can bypass the safety pin")


def test_named_snapshot_restore_commits_config_only_after_atomic_required_state_and_settings() -> None:
    commands = _read("commands/recovery_ipc.rs")
    service = _read("restore_service/orchestration.rs")
    body = _fn_body(commands, "pub async fn restore_db_from_snapshot(", span=14_000)
    history = body.find("state.lock_history().clear();")
    install = body.find("install_snapshot_restore_plan(&restore_plan, &data_dir, &live_controls)?")
    runtime = body.find("*state.lock_settings() = restored.clone();")
    completed = body.find(
        "mark_named_restore_completed(&data_dir, &name, &restore_plan.expected_db_generation_sha256)?;"
    )
    clear = body.find("clear_review_pilot_restore_pending(&data_dir)?;", completed)
    commit = body.find("restore_reservation.commit_named_restore()?;", completed)
    if -1 in (history, install, runtime, completed, clear, commit) or not (
        history < install < runtime < completed < clear < commit
    ):
        raise AssertionError(
            "named restore must install all config/runtime state, durably mark the completed generation, "
            "then clear the marker and release admission in that exact order"
        )
    recovery = _read("recovery.rs")
    install_helper = _fn_body(recovery, "pub(crate) fn install_snapshot_restore_plan(", span=4200)
    routing = install_helper.find("restore_required_snapshot_state_atomic")
    pilot = install_helper.find("apply_snapshot_pilot_policy")
    settings = install_helper.find("restored.save(&live_settings_path)")
    if -1 in (routing, pilot, settings) or not (routing < pilot < settings):
        raise AssertionError("restore-plan installation must atomically bind routing, pilot policy, then typed settings")
    marker = _fn_body(recovery, "pub(crate) fn begin_named_restore_transaction(", span=3800)
    if not (0 <= marker.find("reservation.arm_named_restore()?") < marker.find("write_named_restore_pending")):
        raise AssertionError("named admission must arm fail-closed parking before writing its durable marker")
    for required in (
        "target_db_generation_sha256",
        "completed_db_generation_sha256",
        "NAMED_RESTORE_PENDING_SCHEMA: u32 = 3",
        "fsync_parent_dir_strict",
    ):
        if required not in recovery:
            raise AssertionError(f"named restore lost exact-generation/durable marker authority: {required}")
    if "db.require_restore_generation_sha256(&plan.expected_db_generation_sha256)" not in service:
        raise AssertionError("named restore must prove the exact staged live SQLite generation before config publication")
    if "completed_named_restore_matches_live(data_dir, &pending)?" not in service:
        raise AssertionError("startup cleanup must compare the WAL-aware live generation with the completion digest")

    lib = _read("lib.rs")
    recovery = lib.find("recover_interrupted_named_restore_at_startup(&data_dir)")
    open_db = lib.find("Database::open_with_retry(db_path.to_string_lossy().as_ref())")
    if recovery == -1 or open_db == -1 or recovery >= open_db:
        raise AssertionError("startup must recover a durable restore transaction before opening/initializing the live DB")


def test_long_prework_publishers_hold_full_operation_mutation_guards() -> None:
    commands = _read("commands.rs")
    for signature, final_publish in (
        ("pub async fn import_model_checkpoint(", "register_checkpoint("),
        ("pub async fn import_model_deployment(", "register_verified_deployment_record("),
        ("pub async fn bootstrap_legacy_champion(", "sync_champion_pointer("),
    ):
        body = _fn_body(commands, signature, span=7500)
        worker = body.find("run_blocking(move ||")
        mutation = body.find("database.begin_mutation()")
        mutation_end = body.find(";", mutation)
        writer = body.find("database.lock_after_mutation(&mutation)", mutation)
        publish = body.find(final_publish)
        propagated = mutation != -1 and mutation_end != -1 and "?" in body[mutation:mutation_end]
        if -1 in (worker, mutation, mutation_end, writer, publish) or not propagated or not (
            worker < mutation < writer < publish
        ):
            raise AssertionError(
                f"{signature} must own exact-runtime mutation admission inside its detachable worker through publish"
            )

    integration = _read("integration_runner.rs")
    body = _fn_body(integration, "pub fn run(", span=3500)
    mutation = body.find("crate::database_runtime::begin_mutation()?")
    first_import = body.find("pipeline.import_directory(")
    if mutation == -1 or first_import == -1 or mutation >= first_import:
        raise AssertionError("registered integration/audiobook lifecycle must fence its complete write lifetime")
    if "lock_after_mutation(&mutation)" not in body:
        raise AssertionError("integration lifecycle must not re-enter the ordinary admission lock after mutation")


def test_owner_runtime_writers_enter_exact_mutation_before_waiting_for_sqlite() -> None:
    """A writer may never re-enter the ordinary admission lock after announcing a mutation.

    Holding the admission mutex while waiting for SQLite can hide ``mutations_active`` from a
    competing restore until after the write has committed.  The exact runtime guard plus
    ``lock_after_mutation`` is the lock-order contract that makes writer-first restore refusal
    deterministic.
    """

    def assert_boundary(rel: str, signature: str, span: int = 5000) -> None:
        body = _fn_body(_read(rel), signature, span=span)
        admission = body.find(".begin_mutation()")
        writer = body.find(".lock_after_mutation(", admission)
        if admission == -1 or writer == -1 or admission >= writer:
            raise AssertionError(
                f"{rel}:{signature} must enter its DatabaseRuntime mutation before taking the writer lock"
            )

    for rel, signature, span in (
        ("lib.rs", "pub(crate) fn save_session_view_state(", 1800),
        ("lib.rs", "pub fn session_save(", 1800),
        ("lib.rs", "pub(crate) fn persist_review_cursor(", 1800),
        ("lib.rs", "pub fn session_auto_save(", 1800),
        ("commands.rs", "pub async fn merge_dataset_json(", 2400),
        ("commands/gold_eval.rs", "pub async fn import_gold_segments(", 2600),
        ("commands/gold_eval.rs", "pub async fn run_gold_eval(", 1800),
        ("commands/gold_eval.rs", "pub async fn create_gold_from_file(", 2200),
        ("commands/gold_eval.rs", "pub async fn import_verified_segments_as_gold(", 2000),
        ("commands/jury.rs", "pub async fn run_t0_gate(", 2300),
        ("commands/recovery_ipc.rs", "pub async fn db_vacuum(", 1500),
        ("commands/segments_read.rs", "pub async fn relink_audio(", 2500),
        ("commands/system_ops.rs", "pub async fn import_model_checkpoint(", 4800),
        ("commands/system_ops.rs", "pub async fn import_model_deployment(", 7000),
        ("commands/system_ops.rs", "pub async fn bootstrap_legacy_champion(", 7000),
        ("commands/system_ops.rs", "pub async fn undo(", 3600),
        ("commands/system_ops.rs", "pub async fn redo(", 3600),
        ("commands/system_ops.rs", "pub async fn compute_acoustic_scores(", 9000),
        ("commands/system_ops.rs", "pub async fn compute_signal_anomaly_scores(", 9000),
        ("commands/transcribe.rs", "pub async fn transcribe_segment(", 13_000),
        ("commands/transcribe.rs", "pub async fn align_segment(", 9000),
    ):
        assert_boundary(rel, signature, span)

    ingest = _fn_body(_read("commands/ingest.rs"), "fn emit_agent_stage_event(", span=3400)
    for required in ("app_state.db_runtime()", "database.begin_mutation()", "database.lock_after_mutation(&mutation)"):
        if required not in ingest:
            raise AssertionError(f"agent-stage persistence bypasses exact restore admission: {required}")

    for rel in ("stores/import_write.rs", "stores/jobs.rs"):
        production = _read(rel).split("#[cfg(test)]", 1)[0]
        for required in ("self.runtime.begin_mutation()", "self.runtime.lock_after_mutation(mutation)"):
            if required not in production:
                raise AssertionError(f"{rel} lost the exact runtime writer boundary: {required}")
        if "use crate::database_runtime::{begin_mutation" in production:
            raise AssertionError(f"{rel} regressed to the process-global mutation authority")
        if "self.runtime.lock()" in production:
            raise AssertionError(f"{rel} still has an ordinary writer-lock path")

    segment_store = _read("stores/segment_write.rs").split("#[cfg(test)]", 1)[0]
    if "self.runtime.lock_after_mutation(mutation)" not in segment_store:
        raise AssertionError("segment writes may not re-enter DatabaseRuntime::lock after mutation admission")
    if "self.runtime.lock()" in segment_store:
        raise AssertionError("segment write production code still has an ordinary writer-lock path")

    batch_store = _read("stores/batch.rs").split("#[cfg(test)]", 1)[0]
    if "lock_after_mutation" not in batch_store or ".lock(" in batch_store:
        raise AssertionError("durable batch writes must use their held mutation lease for every writer lock")

    draft_store = _read("stores/review_draft.rs").split("#[cfg(test)]", 1)[0]
    if "with_reserved_review_draft_write" not in draft_store or "lock_after_mutation(mutation)" not in draft_store:
        raise AssertionError("reserved draft writes must pass their mutation capability into the writer lock")
    if "self.runtime.lock()" in draft_store:
        raise AssertionError("reserved draft writes still re-enter the ordinary admission lock")

    for regression in (
        "import_write_and_restore_admission_are_linearized_before_the_writer_lock",
        "session_view_save_and_restore_admission_are_linearized_before_the_writer_lock",
    ):
        if regression not in _read("stores/import_write.rs") + _read("lib.rs"):
            raise AssertionError(f"missing deterministic writer-first/restore-first regression: {regression}")


def test_desktop_playback_writes_are_generation_bound_and_race_proven() -> None:
    review_store = _read("stores/review_write.rs")

    boundaries = (
        (
            "pub(crate) fn begin_desktop_playback_session_at_generation_v1(",
            "self.begin_mutation_at_generation(restore_generation",
            'self.lock_after_mutation("begin_desktop_playback_session_v1", &mutation)',
        ),
        (
            "pub(crate) fn cancel_desktop_playback_session_v1(",
            'self.begin_mutation("cancel_desktop_playback_session_v1")',
            'self.lock_after_mutation("cancel_desktop_playback_session_v1", &mutation)',
        ),
        (
            "pub(crate) fn finalize_desktop_playback_session_at_generation_v1(",
            "self.runtime.begin_mutation_at_restore_generation_serial(restore_generation)",
            'self.lock_after_mutation("finalize_desktop_playback_session_v1", &mutation)',
        ),
    )
    for signature, admission_token, writer_token in boundaries:
        body = _fn_body(review_store, signature, span=1800)
        admission = body.find(admission_token)
        writer = body.find(writer_token, admission)
        if admission == -1 or writer == -1 or admission >= writer:
            raise AssertionError(
                f"{signature} must bind restore admission before taking the desktop playback writer lock"
            )

    for regression in (
        "restore_first_refuses_desktop_playback_begin_finalize_and_cancel_without_mutation",
        "desktop_playback_mutations_block_restore_until_begin_finalize_and_cancel_commit",
        "restored_generation_cannot_finalize_or_replay_an_unfinalized_desktop_session",
    ):
        if regression not in review_store:
            raise AssertionError(f"missing deterministic desktop playback/restore regression: {regression}")


def test_external_writers_share_the_desktop_instance_lock() -> None:
    checks = (
        ("bin/export_pack.rs", "InstanceLock::try_lock", "Database::open_with_retry"),
        ("bin/realign_segments.rs", "InstanceLock::try_lock", "Database::open("),
        ("bin/backfill_fingerprints.rs", "InstanceLock::try_lock", "Database::open("),
        ("bin/reject_speaker_change_clips.rs", "InstanceLock::try_lock", "Database::open("),
        ("bin/speaker_change_probe.rs", "InstanceLock::try_lock", "Connection::open_with_flags"),
    )
    for rel, lock_token, open_token in checks:
        src = _read(rel)
        lock = src.find(lock_token)
        opened = src.find(open_token)
        if lock == -1 or opened == -1 or lock >= opened:
            raise AssertionError(f"{rel} must acquire cortex.lock before opening the generation it may mutate")

    scripts = REPO_ROOT / "scripts"
    for name in ("repair_unfinalized_reviews.py", "requeue_unheard_decisions.py"):
        src = (scripts / name).read_text(encoding="utf-8")
        branch = src.find("if args.apply:")
        lock = src.find("with acquire_cortex_lock", branch)
        dispatch = src.find("return run(args)", lock)
        if -1 in (branch, lock, dispatch) or not (branch < lock < dispatch):
            raise AssertionError(f"{name} must lock the live generation for every apply path")
    focus = (scripts / "activate_voice_focus.py").read_text(encoding="utf-8")
    condition = focus.find("if not (args.merge_import_job and args.dry_run):")
    lock = focus.find("with acquire_cortex_lock(args.data_dir):", condition)
    dispatch = focus.find("return run(args, parser)", lock)
    if -1 in (condition, lock, dispatch) or not (condition < lock < dispatch):
        raise AssertionError("every mutating voice-focus mode must hold cortex.lock across validation and publication")


def _assert_guarded(src: str, signature_start: str, who: str, token: str = "restore_pending()") -> None:
    body = _fn_body(src, signature_start)
    if token not in body:
        raise AssertionError(
            f"{who} does not check {token} before starting — a NEW writer can begin mid-restore and mix "
            f"pre-restore rows into the just-restored library. Add `if {token} {{ return Err(...); }}`."
        )


def test_every_writer_start_checks_restore_pending() -> None:
    lib = _read("lib.rs")
    _assert_guarded(lib, "pub fn try_start_import(", "try_start_import")
    _assert_guarded(
        lib,
        "pub(crate) fn try_start_batch_for_run(",
        "try_start_batch_for_run",
    )

    commands = _read("commands.rs")
    # Follow the command signature rather than one formatting-specific rate-limiter expression. The
    # typed IPC conversion split `.check(...)` across lines; the restore proof still has to inspect
    # the actual function body and find its pre-publication reservation check.
    _assert_guarded(commands, "pub fn run_wsl_refinement(", "run_wsl_refinement (7B refine)")

    jury = _read("commands/jury.rs")
    _assert_guarded(jury, "pub async fn run_dpo_update(", "run_dpo_update", "super::restore_pending()")
    _assert_guarded(jury, "pub async fn run_jury_pipeline(", "run_jury_pipeline", "super::restore_pending()")
    _assert_guarded(jury, "pub async fn run_t2_for_segment(", "run_t2_for_segment", "super::restore_pending()")

    couch = _read("couch.rs")
    # `start_on_port` is now the production-only adapter that supplies the durable-session lifecycle
    # callbacks. Pin its exact delegation so the restore assertion below follows the function that
    # ACTUALLY takes the COUCH lock instead of going stale whenever this seam is refactored.
    start_on_port = _fn_body(couch, "fn start_on_port(", span=900)
    lifecycle_delegate = """start_on_port_with_session_lifecycle(
        db_path,
        reviewers,
        port,
        data_dir,
        save_session_snapshot,
        clear_session_revocation,
    )"""
    if lifecycle_delegate not in start_on_port:
        raise AssertionError(
            "couch::start_on_port must delegate directly to start_on_port_with_session_lifecycle with "
            "the production session callbacks; otherwise this gate may inspect a helper production no longer calls."
        )

    # The lifecycle helper owns the check+register critical section. The lock MUST be acquired before
    # restore_pending() is read and held until the handle is registered; checking in either adapter
    # would reopen the race between the reservation and the background writer becoming visible.
    lifecycle = _fn_body(couch, "fn start_on_port_with_session_lifecycle", span=30_000)
    lock = lifecycle.find("let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());")
    pending = lifecycle.find("if crate::database_runtime::restore_pending()")
    register = lifecycle.find("*guard = Some(handle);")
    if -1 in (lock, pending, register) or not (lock < pending < register):
        raise AssertionError(
            "couch::start_on_port_with_session_lifecycle must acquire COUCH, refuse while "
            "restore_pending(), and register the server handle under that same lock."
        )
    if "drop(guard)" in lifecycle[lock:register]:
        raise AssertionError(
            "couch::start_on_port_with_session_lifecycle drops COUCH before registering the handle, "
            "reopening the restore reservation race."
        )
    # Must match `start`'s ACTUAL one-line body. This literal was written against the pre-`data_dir`
    # signature and silently stopped matching when durable sessions added that parameter — so the gate
    # raised on every run instead of checking anything, which is a broken gate, not a strict one. Keep
    # it exact: a loose `"start_on_port(" in couch` would pass even if `start` grew a real body, which
    # is precisely the bypass this check exists to catch.
    # `configured_port()` replaced the bare `COUCH_PORT` when CORTEX_COUCH_PORT was added so an
    # end-to-end harness could drive the real server without fighting the owner's own for 8737. The
    # literal is updated to the new body EXACTLY, which is what the note below instructs — `start` is
    # still a one-line delegate and the guarded lifecycle helper remains its only production route.
    if "start_on_port(db_path, reviewers, configured_port(), data_dir)" not in couch:
        raise AssertionError(
            "couch::start must delegate to start_on_port so the guarded path is the only way in; if it "
            "grew its own body, this gate is checking a function the app no longer calls. (If you "
            "changed start's signature, update this literal to its new one-line body — do not loosen it.)"
        )
    # `resume` is the OTHER way into a running server — it is what the app calls at launch, unattended,
    # every time the watchdog relaunches. It must reach the guarded function by the same delegate route.
    # Same `configured_port()` change, and it MUST be the same on both: a resume that came back on a
    # different port than `start` binds would resurrect the session at a URL the owner's bookmark
    # does not point at.
    if "resume_on_port(db_path, data_dir, configured_port())" not in couch:
        raise AssertionError(
            "couch::resume must delegate to resume_on_port, which must reach start_on_port — otherwise "
            "the automatic launch path could start a writer without the restore guard."
        )


def main() -> None:
    test_prepare_restore_reserves_before_the_fence_and_returns_the_guard()
    test_restore_service_owns_sql_authority_without_ui_dependencies()
    test_both_restore_callers_hold_the_reservation()
    test_restore_admission_is_exclusive_and_all_appstate_handles_delegate()
    test_snapshot_and_restore_share_one_mutex_guard_in_both_commands()
    test_every_production_database_entrypoint_requires_the_shared_pre_migration_pin()
    test_named_snapshot_restore_commits_config_only_after_atomic_required_state_and_settings()
    test_long_prework_publishers_hold_full_operation_mutation_guards()
    test_owner_runtime_writers_enter_exact_mutation_before_waiting_for_sqlite()
    test_desktop_playback_writes_are_generation_bound_and_race_proven()
    test_external_writers_share_the_desktop_instance_lock()
    test_every_writer_start_checks_restore_pending()
    print("restore-reservation gate source policy passed")


if __name__ == "__main__":
    main()
