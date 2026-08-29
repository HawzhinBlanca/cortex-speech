//! AppState- and Tauri-free restore transaction orchestration.

use super::authority::{has_durable_review_activity, require_restore_authority_superset};
use super::pilot::require_active_pilot_policy_binding;
use super::playback::validate_restore_target_semantics;
use crate::database_runtime::{RestoreAdmission, RestoreReservation, RESTORE_ADMISSION};
use crate::recovery::{
    begin_named_restore_transaction, bind_named_restore_target_generation, clear_review_pilot_restore_pending,
    completed_named_restore_matches_live, explicit_snapshot_pilot_policy, install_snapshot_restore_plan,
    load_named_restore_pending, mark_named_restore_completed, prepare_named_restore_artifacts,
    reopen_named_restore_completion_for_recovery, strict_live_settings_for_restore,
    take_mandatory_pre_restore_snapshot, SnapshotPilotPolicyRestore, SnapshotRestorePlan,
};
use crate::settings::AppSettings;
use std::path::Path;

/// With one caller-owned DB mutex guard, pin the current live database and then replace it. Keeping
/// both operations in this helper prevents a queued write from landing after the safety snapshot and
/// being silently discarded by the restore.
pub(crate) fn restore_with_mandatory_snapshot(
    reservation: &RestoreReservation<'_>,
    db: &mut crate::db::Database,
    data_dir: &Path,
    source: &Path,
) -> Result<std::path::PathBuf, String> {
    // Prove and fully migrate the source in isolation first. A bad source creates neither a safety
    // pin nor a durable review barrier and cannot touch a live page.
    let staged = crate::db::Database::stage_restore_source(source).map_err(|error| error.to_string())?;
    if has_durable_review_activity(db)? || has_durable_review_activity(&staged)? {
        return Err(
            "Bare database restore is refused when either the live or target generation contains durable review activity. Use a named recovery snapshot so database, pilot policy, and routing state restore as one verified generation."
                .to_string(),
        );
    }
    require_restore_authority_superset(db, &staged)?;
    validate_restore_target_semantics(&staged)?;
    let pinned = take_mandatory_pre_restore_snapshot(reservation, db, data_dir)?;
    tracing::info!("pre-restore snapshot pinned at {}", pinned.display());
    let expected = staged.restore_generation_sha256().map_err(|error| {
        format!("bare restore target generation could not be canonicalized before publication: {error}")
    })?;
    db.commit_staged_restore(&staged).map_err(|error| error.to_string())?;
    db.require_restore_generation_sha256(&expected)
        .map_err(|error| format!("bare restore publication did not produce the exact staged generation: {error}"))?;
    Ok(pinned)
}

pub(crate) fn prepare_and_restore_named_transaction(
    reservation: &RestoreReservation<'_>,
    db: &mut crate::db::Database,
    data_dir: &Path,
    snapshot_dir: &Path,
    source: &Path,
    source_selector: &str,
) -> Result<SnapshotRestorePlan, String> {
    let (plan, staged) = prepare_named_restore_artifacts(snapshot_dir, source, || {})?;
    if let Some(pending) = load_named_restore_pending(data_dir)? {
        if pending.completed_selector.is_some() {
            return Err("the named restore already completed; only durable barrier cleanup may run".to_string());
        }
        if pending.source_selector != source_selector {
            return Err(format!(
                "an interrupted restore of '{}' is pending; retry that exact snapshot before selecting '{}'",
                pending.source_selector, source_selector
            ));
        }
        // The live connection may already contain the target (or a partial prior publication). The
        // only authoritative pre-restore floor is the original manifest-verified pin recorded before
        // the first swap; stage that pin independently and compare against it on every retry.
        let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)?;
        let original_source = original_pin.join("cortex-speech.db");
        let (floor_plan, floor) = prepare_named_restore_artifacts(&original_pin, &original_source, || {})
            .map_err(|error| format!("interrupted restore's original safety floor is unusable: {error}"))?;
        let floor_policy = explicit_snapshot_pilot_policy(&floor_plan.pilot, "original safety floor")?;
        require_restore_authority_superset(&floor, &staged)?;
        require_active_pilot_policy_binding(&floor, floor_policy.as_ref(), &staged, &plan.pilot)?;
    } else {
        // No transaction has crossed its marker yet, so the locked live DB and its live policy are
        // the exact authoritative floor. Admission + the caller-owned DB mutex keep that floor fixed
        // through comparison, pin creation, marker commit, and page publication.
        let floor_policy = crate::review_pilot::load(data_dir)?;
        require_restore_authority_superset(db, &staged)?;
        require_active_pilot_policy_binding(db, floor_policy.as_ref(), &staged, &plan.pilot)?;
    }
    validate_restore_target_semantics(&staged)?;
    let _pin = begin_named_restore_transaction(
        reservation,
        db,
        data_dir,
        source_selector,
        &plan.expected_db_generation_sha256,
    )?;
    db.commit_staged_restore(&staged).map_err(|error| error.to_string())?;
    db.require_restore_generation_sha256(&plan.expected_db_generation_sha256)
        .map_err(|error| format!("named restore publication did not produce the exact staged generation: {error}"))?;
    Ok(plan)
}

/// Shared restore precondition (true-10 audit 2026-07-09): refuse while an import/batch worker may
/// be writing, and pin a rotation-exempt copy of the CURRENT live DB first so a mis-restore of the
/// wrong snapshot is itself recoverable (previously only from a ≤10-min rolling snapshot that
/// rotated out within ~100 minutes). Returns a RestoreReservation the caller MUST hold across the
/// restore so no new writer can start mid-restore (P1.3b).
fn publish_prepared_snapshot_generation_offline(
    data_dir: &Path,
    restore_plan: &SnapshotRestorePlan,
    staged: &crate::db::Database,
    live_controls: &AppSettings,
) -> Result<String, String> {
    validate_restore_target_semantics(staged)?;
    let db_path = data_dir.join("cortex-speech.db");
    let mut live = crate::db::Database::open_with_retry(db_path.to_string_lossy().as_ref())
        .map_err(|error| format!("could not open live database for recovery: {error}"))?;
    live.commit_staged_restore(staged)
        .map_err(|error| format!("could not publish recovered database generation: {error}"))?;
    live.require_restore_generation_sha256(&restore_plan.expected_db_generation_sha256)
        .map_err(|error| format!("recovered live database is not the exact staged generation: {error}"))?;
    let integrity =
        live.integrity_check().map_err(|error| format!("recovered live database could not be verified: {error}"))?;
    if integrity.trim() != "ok" {
        return Err(format!("recovered live database failed integrity_check: {integrity}"));
    }
    drop(live);
    install_snapshot_restore_plan(restore_plan, data_dir, live_controls)?;
    Ok(restore_plan.expected_db_generation_sha256.clone())
}

fn restore_snapshot_generation_offline(
    data_dir: &Path,
    selector: &str,
    live_controls: &AppSettings,
    authoritative_floor: &crate::db::Database,
    authoritative_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
    bind_recorded_target: bool,
) -> Result<String, String> {
    let snapshot_dir = crate::snapshot::resolve_snapshot_dir(data_dir, selector)?;
    let source = snapshot_dir.join("cortex-speech.db");
    let source_metadata = std::fs::symlink_metadata(&source)
        .map_err(|error| format!("snapshot '{selector}' has no readable database file: {error}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{selector}' has no regular database file"));
    }
    let (restore_plan, staged) = prepare_named_restore_artifacts(&snapshot_dir, &source, || {})?;
    require_restore_authority_superset(authoritative_floor, &staged)?;
    require_active_pilot_policy_binding(authoritative_floor, authoritative_policy, &staged, &restore_plan.pilot)?;
    if bind_recorded_target {
        // This occurs after complete source/config/semantic validation but before the first live page
        // can change. A replaced snapshot directory under the same selector therefore cannot be
        // replayed as the originally recorded target.
        bind_named_restore_target_generation(data_dir, selector, &restore_plan.expected_db_generation_sha256)?;
    }
    publish_prepared_snapshot_generation_offline(data_dir, &restore_plan, &staged, live_controls)
}

/// Complete an interrupted cross-file restore before normal startup performs ANY DB/config write,
/// snapshot, Couch resume, or background work. The intended target is retried first. If that target
/// cannot be made coherent, the manifest-verified original pre-restore generation is restored in
/// full. Both paths keep the durable marker until DB + all required config have committed.
pub(crate) fn recover_interrupted_named_restore_with_admission(
    data_dir: &Path,
    admission: &RestoreAdmission,
) -> Result<bool, String> {
    let Some(mut pending) = load_named_restore_pending(data_dir)? else {
        return Ok(false);
    };
    let reservation = admission.claim_recovery()?;
    if let Some(completed_selector) = pending.completed_selector.as_deref() {
        // A healthy database is not proof of the recorded generation. Only a WAL-aware, canonical
        // digest match may take the cleanup-only path. Legacy markers and mismatches are reopened as
        // incomplete and replay the exact target/original transaction below while the barrier stays.
        if completed_named_restore_matches_live(data_dir, &pending)? {
            strict_live_settings_for_restore(&data_dir.join("settings.json"))?;
            clear_review_pilot_restore_pending(data_dir)?;
            reservation.commit_named_restore()?;
            tracing::warn!(
                "finished exact-generation marker cleanup for already-completed restore '{completed_selector}' before startup"
            );
            return Ok(true);
        }
        tracing::error!(
            "completed restore '{completed_selector}' does not match its recorded SQLite generation; replaying the exact target/original transaction"
        );
        pending = reopen_named_restore_completion_for_recovery(data_dir, &pending)?;
    }
    let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)
        .map_err(|error| format!("interrupted restore has no usable original safety pin: {error}"))?;
    if !crate::snapshot::verify_snapshot_manifest_for_restore(&original_pin)? {
        return Err(
            "interrupted restore's original safety pin is legacy/unverifiable; refusing normal startup".to_string()
        );
    }
    // Stage the recorded original pin once and keep that owned in-memory generation as the floor for
    // BOTH target retry and fallback. Never consult possibly-swapped live pages for admission, and do
    // not re-migrate the original twice (time-derived migration values could otherwise differ).
    let original_source = original_pin.join("cortex-speech.db");
    let (original_plan, original_floor) = prepare_named_restore_artifacts(&original_pin, &original_source, || {})?;
    if matches!(original_plan.pilot, SnapshotPilotPolicyRestore::PreserveLegacy)
        || original_plan
            .optional
            .iter()
            .any(|(_, action)| matches!(action, crate::snapshot::OptionalSnapshotRestore::PreserveLegacy))
    {
        return Err("interrupted restore's original safety pin does not explicitly bind every required config state"
            .to_string());
    }
    let original_policy = explicit_snapshot_pilot_policy(&original_plan.pilot, "original safety pin")?;

    let settings_path = data_dir.join("settings.json");
    let live_controls = strict_live_settings_for_restore(&settings_path)?;
    let target_result = restore_snapshot_generation_offline(
        data_dir,
        &pending.source_selector,
        &live_controls,
        &original_floor,
        original_policy.as_ref(),
        true,
    );
    let (completed_selector, completed_digest) = match target_result {
        Ok(digest) => (pending.source_selector.clone(), digest),
        Err(target_error) => {
            tracing::error!(
                "interrupted target restore '{}' could not complete ({target_error}); rolling back verified original '{}'",
                pending.source_selector,
                pending.pre_restore_pin_selector
            );
            require_restore_authority_superset(&original_floor, &original_floor)
                .and_then(|()| {
                    require_active_pilot_policy_binding(
                        &original_floor,
                        original_policy.as_ref(),
                        &original_floor,
                        &original_plan.pilot,
                    )
                })
                .and_then(|()| {
                    publish_prepared_snapshot_generation_offline(
                        data_dir,
                        &original_plan,
                        &original_floor,
                        &live_controls,
                    )
                })
                .map_err(|rollback_error| {
                    format!(
                        "interrupted restore could not complete target '{}' ({target_error}) and could not roll back verified original '{}' ({rollback_error}); normal startup is blocked",
                        pending.source_selector, pending.pre_restore_pin_selector
                    )
                })?;
            (pending.pre_restore_pin_selector.clone(), original_plan.expected_db_generation_sha256.clone())
        }
    };
    // Marker deletion is outside the fallback branch: failure here means the selected generation is
    // already coherent, so rolling it back would be an unnecessary second data transition. Stay fatal
    // and retry marker cleanup idempotently on the next launch.
    mark_named_restore_completed(data_dir, &completed_selector, &completed_digest)?;
    clear_review_pilot_restore_pending(data_dir)?;
    reservation.commit_named_restore()?;
    tracing::warn!("completed interrupted restore recovery using '{completed_selector}' before normal startup");
    Ok(true)
}

pub(crate) fn recover_interrupted_named_restore_at_startup(data_dir: &Path) -> Result<bool, String> {
    recover_interrupted_named_restore_with_admission(data_dir, RESTORE_ADMISSION.as_ref())
}
