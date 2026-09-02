//! Tauri-free restore admission and durable cross-generation marker authority.
//!
//! Restore commands adapt `AppState` into this module, but process-wide admission ordering,
//! fail-closed marker recovery and atomic marker publication belong here. Semantic database/config
//! validation is still being strangled out of `commands.rs` in later slices.

use crate::database_runtime::{RestoreAdmission, RestoreReservation, RESTORE_ADMISSION};
use crate::settings::AppSettings;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnapshotPilotPolicyRestore {
    Install(Vec<u8>),
    ExplicitlyAbsent,
    /// Snapshots made before explicit absence markers preserve the current live policy. This keeps
    /// historical DB recovery possible without ever interpreting missing legacy state as permission
    /// to delete/relax a live paid-review cap.
    PreserveLegacy,
}

#[derive(Debug, Clone)]
pub(crate) struct SnapshotRestorePlan {
    pub(crate) pilot: SnapshotPilotPolicyRestore,
    pub(crate) optional: Vec<(crate::snapshot::OptionalSnapshotState, crate::snapshot::OptionalSnapshotRestore)>,
    /// Canonical logical digest of the fully migrated, validated SQLite generation paired with this
    /// configuration plan. Config publication and the durable completion marker may advance only
    /// after the live WAL-aware database proves this exact value.
    pub(crate) expected_db_generation_sha256: String,
}

pub(crate) fn inspect_snapshot_pilot_policy(
    snapshot_dir: &Path,
    original_schema_version: i64,
    original_max_review_event_id: i64,
    manifest_verified: bool,
) -> Result<SnapshotPilotPolicyRestore, String> {
    let policy_path = snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE);
    let absent_path = snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE);
    let read_optional = |path: &Path| -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("snapshot state {} is unreadable: {error}", path.display())),
        }
    };
    match (read_optional(&policy_path)?, read_optional(&absent_path)?) {
        (Some(_), Some(_)) => Err(format!(
            "snapshot is ambiguous: it contains both {} and {}",
            crate::review_pilot::REVIEW_PILOT_FILE,
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
        )),
        (Some(bytes), None) => {
            if !manifest_verified {
                return Err(
                    "policy-bearing named snapshot restore requires a verified manifest that cryptographically binds its database, policy, and exact voice focus"
                        .to_string(),
                );
            }
            let raw = std::str::from_utf8(&bytes).map_err(|error| {
                format!("snapshot {} is not UTF-8: {error}", crate::review_pilot::REVIEW_PILOT_FILE)
            })?;
            let policy = crate::review_pilot::parse(raw)?;
            // A policy-bearing artifact is one indivisible DB + policy + exact-focus generation.
            // This applies equally to manifestless legacy snapshots: preserving or inferring a
            // missing focus would turn a bounded campaign into a different paid workload.
            crate::review_pilot::validate_controlled_focus(snapshot_dir)
                .map_err(|error| format!("snapshot controlled-pilot focus is invalid: {error}"))?;
            if original_schema_version < crate::review_pilot::REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION {
                return Err(format!(
                    "policy-bearing snapshot schema {original_schema_version} predates durable hidden-key authority v{}; restoring it could forget already-served paid QC keys",
                    crate::review_pilot::REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION
                ));
            }
            if policy.after_review_event_id > original_max_review_event_id {
                return Err(format!(
                    "snapshot pilot baseline {} is ahead of its database review-event maximum {original_max_review_event_id}",
                    policy.after_review_event_id
                ));
            }
            let mut canonical = serde_json::to_vec_pretty(&policy)
                .map_err(|error| format!("snapshot pilot policy could not be canonicalized: {error}"))?;
            canonical.push(b'\n');
            Ok(SnapshotPilotPolicyRestore::Install(canonical))
        }
        (None, Some(marker)) => {
            if marker != crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES {
                return Err(format!(
                    "snapshot {} has invalid contents",
                    crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
                ));
            }
            Ok(SnapshotPilotPolicyRestore::ExplicitlyAbsent)
        }
        (None, None) => {
            if manifest_verified {
                return Err(format!(
                    "manifest-bearing snapshot is missing both {} and {}",
                    crate::review_pilot::REVIEW_PILOT_FILE,
                    crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
                ));
            }
            tracing::warn!(
                "LEGACY MANIFEST-LESS SNAPSHOT: neither {} nor {} is present; preserving the current live paid-review policy exactly",
                crate::review_pilot::REVIEW_PILOT_FILE,
                crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
            );
            Ok(SnapshotPilotPolicyRestore::PreserveLegacy)
        }
    }
}

pub(crate) fn explicit_snapshot_pilot_policy(
    action: &SnapshotPilotPolicyRestore,
    context: &str,
) -> Result<Option<crate::review_pilot::ReviewPilotPolicy>, String> {
    match action {
        SnapshotPilotPolicyRestore::Install(bytes) => {
            let raw =
                std::str::from_utf8(bytes).map_err(|error| format!("{context} pilot policy is not UTF-8: {error}"))?;
            crate::review_pilot::parse(raw).map(Some)
        }
        SnapshotPilotPolicyRestore::ExplicitlyAbsent => Ok(None),
        SnapshotPilotPolicyRestore::PreserveLegacy => {
            Err(format!("{context} does not explicitly bind paid-review policy presence or absence"))
        }
    }
}

pub(crate) fn inspect_snapshot_restore_plan(
    snapshot_dir: &Path,
    snapshot_db: &crate::db::Database,
    original_schema_version: i64,
    original_max_review_event_id: i64,
    manifest_verified: bool,
) -> Result<SnapshotRestorePlan, String> {
    let pilot = inspect_snapshot_pilot_policy(
        snapshot_dir,
        original_schema_version,
        original_max_review_event_id,
        manifest_verified,
    )?;
    let optional = crate::snapshot::OPTIONAL_SNAPSHOT_STATE
        .iter()
        .copied()
        .map(|state| {
            crate::snapshot::inspect_optional_state_for_restore(snapshot_dir, state, manifest_verified)
                .map(|action| (state, action))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected_db_generation_sha256 = snapshot_db
        .restore_generation_sha256()
        .map_err(|error| format!("snapshot database generation could not be canonicalized for restore: {error}"))?;
    Ok(SnapshotRestorePlan { pilot, optional, expected_db_generation_sha256 })
}

pub(crate) fn prepare_named_restore_artifacts<F>(
    snapshot_dir: &Path,
    source: &Path,
    after_private_capture: F,
) -> Result<(SnapshotRestorePlan, crate::db::Database), String>
where
    F: FnOnce(),
{
    let expected_source = snapshot_dir.join("cortex-speech.db");
    if source != expected_source {
        return Err(format!(
            "named restore database {} is not the database declared by snapshot {}",
            source.display(),
            snapshot_dir.display()
        ));
    }

    // Capture every source artifact through the same stream that computes its digest. The injected
    // boundary runs only after that private copy exists; a whole-tree, DB-only, config-only or
    // in-place byte swap is detected by the typed generation digest before anything is parsed.
    let image = crate::snapshot::VerifiedSnapshotImage::capture(snapshot_dir, after_private_capture)?;
    image.verify_owned_digest()?;

    // These are the only parse/stage paths. Neither config planning nor SQLite migration opens the
    // mutable promoted source again, so a later path replacement cannot mix generations.
    let (staged, original_schema_version, original_max_review_event_id) =
        crate::db::Database::stage_restore_source_with_original_evidence(image.database_path())
            .map_err(|error| error.to_string())?;
    image.verify_owned_digest()?;
    // Bind config planning to the already-owned, WAL-aware, fully migrated SQLite generation. No
    // immutable main-file probe is allowed here: if a source ever carries WAL authority, staging is
    // the only path that may interpret it.
    let plan = inspect_snapshot_restore_plan(
        image.root(),
        &staged,
        original_schema_version,
        original_max_review_event_id,
        image.manifest_verified(),
    )?;
    image.verify_owned_digest()?;
    Ok((plan, staged))
}

pub(crate) fn take_mandatory_pre_restore_snapshot(
    reservation: &RestoreReservation<'_>,
    db: &crate::db::Database,
    data_dir: &Path,
) -> Result<std::path::PathBuf, String> {
    crate::snapshot::take_pinned_snapshot_during_restore(reservation, db, data_dir, "prerestore", 3).map_err(
        |e| {
            format!(
                "Database restore refused because the mandatory pre-restore safety snapshot failed: {e}. \
                 The current library has not been overwritten. Free disk space or fix the destination permissions, then retry."
            )
        },
    )
}

fn pin_selector(data_dir: &Path, pin: &Path) -> Result<String, String> {
    let relative = pin
        .strip_prefix(data_dir.join("snapshots"))
        .map_err(|_| format!("pre-restore pin {} is outside the snapshot tree", pin.display()))?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "pre-restore pin path is not UTF-8".to_string())?;
    if parts.len() != 2 || parts[0] != "pinned" {
        return Err(format!("pre-restore pin {} has an unexpected path", pin.display()));
    }
    Ok(format!("pinned/{}", parts[1]))
}

/// Reuse the original safety pin for every retry of an interrupted named restore. Creating a new
/// `keep=3` pin per retry could evict the only copy of the true pre-restore generation.
pub(crate) fn begin_named_restore_transaction(
    reservation: &RestoreReservation<'_>,
    db: &crate::db::Database,
    data_dir: &Path,
    source_selector: &str,
    target_db_generation_sha256: &str,
) -> Result<std::path::PathBuf, String> {
    validate_generation_sha256(target_db_generation_sha256)?;
    if let Some(pending) = load_named_restore_pending(data_dir)? {
        if let Some(completed) = pending.completed_selector.as_deref() {
            return Err(format!(
                "restore generation '{completed}' is already complete; only durable barrier cleanup may run"
            ));
        }
        if pending.source_selector != source_selector {
            return Err(format!(
                "an interrupted restore of '{}' is pending; retry that exact snapshot before selecting '{}'",
                pending.source_selector, source_selector
            ));
        }
        if let Some(recorded) = pending.target_db_generation_sha256.as_deref() {
            if recorded != target_db_generation_sha256 {
                return Err(format!(
                    "the pending restore target generation changed: recorded {recorded}, staged {target_db_generation_sha256}; refusing to replay a different database under the same selector"
                ));
            }
        } else {
            // Upgrade an interrupted schema-1/2 marker before another page publication. The old
            // marker remains a valid fail-closed barrier, but it cannot authorize a second swap until
            // the exact staged target generation is durably bound.
            let upgraded = NamedRestorePending {
                schema: NAMED_RESTORE_PENDING_SCHEMA,
                target_db_generation_sha256: Some(target_db_generation_sha256.to_string()),
                ..pending.clone()
            };
            replace_named_restore_pending(data_dir, &upgraded)?;
        }
        let pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)?;
        if !crate::snapshot::verify_snapshot_manifest_for_restore(&pin)? {
            return Err(
                "the pending restore's original safety pin is legacy/unverifiable; refusing to continue".to_string()
            );
        }
        tracing::info!("reusing interrupted restore safety pin at {}", pin.display());
        return Ok(pin);
    }
    let pin = take_mandatory_pre_restore_snapshot(reservation, db, data_dir)?;
    let pending = NamedRestorePending {
        schema: NAMED_RESTORE_PENDING_SCHEMA,
        source_selector: source_selector.to_string(),
        pre_restore_pin_selector: pin_selector(data_dir, &pin)?,
        target_db_generation_sha256: Some(target_db_generation_sha256.to_string()),
        completed_selector: None,
        completed_db_generation_sha256: None,
    };
    // This is the commit boundary: source/config preflight and the safety pin already succeeded;
    // the durable fail-closed marker lands immediately before the live SQLite page transaction.
    reservation.arm_named_restore()?;
    if let Err(error) = write_named_restore_pending(data_dir, &pending) {
        if !named_restore_barrier_may_exist(data_dir) {
            reservation.disarm_named_restore_if_safe();
        }
        return Err(error);
    }
    Ok(pin)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NamedRestorePending {
    pub(crate) schema: u32,
    pub(crate) source_selector: String,
    pub(crate) pre_restore_pin_selector: String,
    /// Schema 3 binds the exact fully migrated target before any live page is published. Legacy
    /// schema-1/2 markers omit this and are upgraded or replayed fail-closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_db_generation_sha256: Option<String>,
    /// Written only after DB + every required config/settings file has committed. If marker cleanup
    /// then fails or the process crashes, startup clears the barrier without replaying or rolling
    /// back an already-coherent generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_selector: Option<String>,
    /// Exact canonical live SQLite generation proven after FULL publication and before completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_db_generation_sha256: Option<String>,
}

pub(crate) const NAMED_RESTORE_PENDING_SCHEMA: u32 = 3;

fn validate_generation_sha256(digest: &str) -> Result<(), String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(
            "restore database generation SHA-256 must be exactly 64 lowercase hexadecimal characters".to_string()
        );
    }
    Ok(())
}

pub(crate) fn atomic_write_restore_state(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            return Err(format!("restore destination {} must be a regular file or absent", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect restore destination {}: {error}", path.display())),
    }
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("restore-state");
    let temp = path.with_file_name(format!(".{file_name}.restore-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if let Err(error) = std::fs::write(&temp, bytes) {
        return Err(format!("could not stage {}: {error}", path.display()));
    }
    if let Err(error) = crate::atomic_file::replace_file(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not atomically install {}: {error}", path.display()));
    }
    crate::atomic_file::fsync_parent_dir_strict(path)
        .map_err(|error| format!("could not durably publish restore state {}: {error}", path.display()))?;
    Ok(())
}

pub(crate) fn remove_live_restore_state(destination: &Path) -> Result<(), String> {
    crate::atomic_file::recover_interrupted_replace(destination)
        .map_err(|error| format!("could not recover {} before explicit removal: {error}", destination.display()))?;
    // Remove recoverable backups FIRST while the canonical file still exists. If cleanup is blocked
    // by an antivirus/indexer lock, returning here leaves the old committed state intact; deleting
    // canonical first could let a leftover backup resurrect it after we had reported absence.
    crate::atomic_file::remove_replacement_backups(destination)
        .map_err(|error| format!("could not remove stale backups for {}: {error}", destination.display()))?;
    match std::fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not remove {}: {error}", destination.display())),
    }
    crate::atomic_file::fsync_parent_dir_strict(destination)
        .map_err(|error| format!("could not durably persist absence of {}: {error}", destination.display()))?;
    Ok(())
}

fn validate_named_restore_pending(state: &NamedRestorePending) -> Result<(), String> {
    if !matches!(state.schema, 1 | 2 | NAMED_RESTORE_PENDING_SCHEMA) {
        return Err(format!("unsupported restore transaction schema {}", state.schema));
    }
    for (label, selector) in
        [("source", state.source_selector.as_str()), ("pre-restore pin", state.pre_restore_pin_selector.as_str())]
    {
        if selector.is_empty()
            || selector.trim() != selector
            || selector.len() > 255
            || selector.chars().any(char::is_control)
        {
            return Err(format!("restore transaction {label} selector is invalid"));
        }
    }
    if state.schema == 1 && state.completed_selector.is_some() {
        return Err("legacy restore transaction cannot claim a completed generation".to_string());
    }
    if state.schema < NAMED_RESTORE_PENDING_SCHEMA
        && (state.target_db_generation_sha256.is_some() || state.completed_db_generation_sha256.is_some())
    {
        return Err(format!("legacy restore transaction schema {} cannot carry generation digests", state.schema));
    }
    if let Some(completed) = state.completed_selector.as_deref() {
        if completed != state.source_selector && completed != state.pre_restore_pin_selector {
            return Err("restore transaction completion selector is not its target or original pin".to_string());
        }
    }
    if state.schema == NAMED_RESTORE_PENDING_SCHEMA {
        let target = state.target_db_generation_sha256.as_deref().ok_or_else(|| {
            "restore transaction schema 3 is missing its target database generation SHA-256".to_string()
        })?;
        validate_generation_sha256(target)?;
        match (state.completed_selector.as_deref(), state.completed_db_generation_sha256.as_deref()) {
            (None, None) => {}
            (Some(selector), Some(completed)) => {
                validate_generation_sha256(completed)?;
                if selector == state.source_selector && completed != target {
                    return Err(
                        "restore transaction claims target completion with a different database generation SHA-256"
                            .to_string(),
                    );
                }
            }
            (Some(_), None) => {
                return Err("restore transaction completion is missing its database generation SHA-256".to_string());
            }
            (None, Some(_)) => {
                return Err("restore transaction has a completion digest without a completion selector".to_string());
            }
        }
    }
    Ok(())
}

pub(crate) fn load_named_restore_pending(data_dir: &Path) -> Result<Option<NamedRestorePending>, String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    crate::atomic_file::recover_interrupted_replace(&pending)
        .map_err(|error| format!("could not recover the paid-review restore barrier: {error}"))?;
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read restore transaction {}: {error}", pending.display())),
    };
    let state: NamedRestorePending = serde_json::from_slice(&bytes).map_err(|error| {
        format!("restore transaction {} is invalid and paid review remains blocked: {error}", pending.display())
    })?;
    validate_named_restore_pending(&state)?;
    Ok(Some(state))
}

/// Conservatively decide whether dropping a restore command must keep the process-wide admission
/// fence parked. Invalid/unreadable marker state is recovery-required too: uncertainty can never be
/// interpreted as permission to resume writes.
pub(crate) fn named_restore_barrier_may_exist(data_dir: &Path) -> bool {
    match load_named_restore_pending(data_dir) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            tracing::error!("restore barrier is invalid or unreadable; keeping database fenced: {error}");
            true
        }
    }
}

pub(crate) fn write_named_restore_pending(data_dir: &Path, state: &NamedRestorePending) -> Result<(), String> {
    validate_named_restore_pending(state)?;
    if let Some(existing) = load_named_restore_pending(data_dir)? {
        return (existing == *state).then_some(()).ok_or_else(|| {
            format!(
                "another interrupted restore transaction is pending for '{}'; retry that exact snapshot before selecting '{}'",
                existing.source_selector, state.source_selector
            )
        });
    }
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not serialize restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

fn replace_named_restore_pending(data_dir: &Path, state: &NamedRestorePending) -> Result<(), String> {
    validate_named_restore_pending(state)?;
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not serialize restore transaction replacement: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

pub(crate) fn bind_named_restore_target_generation(
    data_dir: &Path,
    source_selector: &str,
    target_db_generation_sha256: &str,
) -> Result<(), String> {
    validate_generation_sha256(target_db_generation_sha256)?;
    let mut pending = load_named_restore_pending(data_dir)?
        .ok_or_else(|| "restore target generation cannot be bound because its durable marker is missing".to_string())?;
    if pending.source_selector != source_selector {
        return Err(format!(
            "restore target generation belongs to selector '{}', not '{}'",
            pending.source_selector, source_selector
        ));
    }
    if pending.completed_selector.is_some() {
        return Err("a completed restore marker cannot be rebound to a target generation".to_string());
    }
    if let Some(existing) = pending.target_db_generation_sha256.as_deref() {
        return (existing == target_db_generation_sha256).then_some(()).ok_or_else(|| {
            format!(
                "the pending restore target generation changed: recorded {existing}, staged {target_db_generation_sha256}"
            )
        });
    }
    pending.schema = NAMED_RESTORE_PENDING_SCHEMA;
    pending.target_db_generation_sha256 = Some(target_db_generation_sha256.to_string());
    replace_named_restore_pending(data_dir, &pending)
}

pub(crate) fn mark_named_restore_completed(
    data_dir: &Path,
    completed_selector: &str,
    completed_db_generation_sha256: &str,
) -> Result<(), String> {
    validate_generation_sha256(completed_db_generation_sha256)?;
    let mut pending = load_named_restore_pending(data_dir)?
        .ok_or_else(|| "restore completion cannot be recorded because its durable marker is missing".to_string())?;
    if completed_selector != pending.source_selector && completed_selector != pending.pre_restore_pin_selector {
        return Err("restore completion selector is not the recorded target or original pin".to_string());
    }
    if let Some(existing) = pending.completed_selector.as_deref() {
        return (existing == completed_selector
            && pending.completed_db_generation_sha256.as_deref() == Some(completed_db_generation_sha256))
        .then_some(())
        .ok_or_else(|| {
            format!("restore was already completed with a different selector or database generation '{existing}'")
        });
    }
    pending.schema = NAMED_RESTORE_PENDING_SCHEMA;
    if pending.target_db_generation_sha256.is_none() {
        pending.target_db_generation_sha256 = Some(completed_db_generation_sha256.to_string());
    }
    if completed_selector == pending.source_selector
        && pending.target_db_generation_sha256.as_deref() != Some(completed_db_generation_sha256)
    {
        return Err("target restore completion digest does not match the transaction's recorded target".to_string());
    }
    pending.completed_selector = Some(completed_selector.to_string());
    pending.completed_db_generation_sha256 = Some(completed_db_generation_sha256.to_string());
    let mut bytes = serde_json::to_vec_pretty(&pending)
        .map_err(|error| format!("could not serialize completed restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

/// Reopen a completed marker only while startup holds recovery admission and has already proved that
/// the live SQLite generation does not match its completion digest. A crash after this rewrite leaves
/// an ordinary incomplete barrier, so the next launch replays the same target/original path again.
pub(crate) fn reopen_named_restore_completion_for_recovery(
    data_dir: &Path,
    pending: &NamedRestorePending,
) -> Result<NamedRestorePending, String> {
    let mut reopened = pending.clone();
    reopened.completed_selector = None;
    reopened.completed_db_generation_sha256 = None;
    replace_named_restore_pending(data_dir, &reopened)?;
    Ok(reopened)
}

pub(crate) fn completed_named_restore_matches_live(
    data_dir: &Path,
    pending: &NamedRestorePending,
) -> Result<bool, String> {
    let Some(expected) = pending.completed_db_generation_sha256.as_deref() else {
        return Ok(false);
    };
    validate_generation_sha256(expected)?;
    let db_path = data_dir.join("cortex-speech.db");
    let live = crate::db::Database::stage_restore_source(&db_path)
        .map_err(|error| format!("completed restore live database could not be verified: {error}"))?;
    Ok(live
        .restore_generation_sha256()
        .map_err(|error| format!("completed restore live database generation could not be digested: {error}"))?
        == expected)
}

pub(crate) fn clear_review_pilot_restore_pending(data_dir: &Path) -> Result<(), String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    // Canonical marker removal is the FINAL commit point. Backups must disappear first; otherwise a
    // cleanup failure after canonical removal could let load-time atomic recovery resurrect a barrier
    // after the in-process admission guard had already been released.
    crate::atomic_file::remove_replacement_backups(&pending).map_err(|error| {
        format!("restore completed, but a stale paid-review restore-barrier backup could not be removed: {error}")
    })?;
    std::fs::remove_file(&pending).map_err(|error| {
        format!(
            "restore completed, but paid review remains fail-closed because {} could not be removed: {error}",
            pending.display()
        )
    })?;
    crate::atomic_file::fsync_parent_dir_strict(&pending).map_err(|error| {
        format!("restore barrier was removed but its directory could not be durably synchronized: {error}")
    })?;
    Ok(())
}

/// Reserve before probing active writers so the writer-side publish/recheck protocol closes the
/// check-then-register race. The callback keeps this ordering testable without importing AppState.
pub(crate) fn prepare_restore_admission(
    data_dir: PathBuf,
    writers_active: impl FnOnce() -> bool,
) -> Result<(RestoreReservation<'static>, PathBuf), String> {
    prepare_restore_admission_with(data_dir, RESTORE_ADMISSION.as_ref(), writers_active)
}

fn prepare_restore_admission_with<'a>(
    data_dir: PathBuf,
    admission: &'a RestoreAdmission,
    writers_active: impl FnOnce() -> bool,
) -> Result<(RestoreReservation<'a>, PathBuf), String> {
    let reservation = if named_restore_barrier_may_exist(&data_dir) {
        admission.claim_recovery()?
    } else {
        admission.try_reserve()?
    };
    if writers_active() {
        return Err(
            "A background write is in progress (import, batch, 7B refinement, jury, or the Couch Review server) — cancel it, let it finish, or stop Couch Review before restoring. Restoring mid-write would mix pre-restore rows into the restored library and re-arm stale undo history."
                .to_string(),
        );
    }
    Ok((reservation, data_dir))
}

pub(crate) fn refuse_bare_restore_during_controlled_pilot(data_dir: &Path) -> Result<(), String> {
    match crate::review_pilot::load(data_dir) {
        Ok(None) => match crate::couch::durable_controlled_pilot_state(data_dir) {
            Ok(false) => Ok(()),
            Ok(true) => Err(
                "Bare database restore is refused because the durable Couch session retains a controlled paid-review baseline. Use a policy-bearing named snapshot restore instead."
                    .to_string(),
            ),
            Err(error) => Err(format!(
                "Bare database restore is refused because durable Couch pilot state is not provably safe: {error}"
            )),
        },
        Ok(Some(_)) => Err(
            "Bare database restore is refused while a controlled paid-review pilot is active: its external baseline/policy would no longer match review_events. Use a policy-bearing named snapshot restore instead."
                .to_string(),
        ),
        Err(error) => Err(format!(
            "Bare database restore is refused because controlled paid-review state is not provably safe: {error}"
        )),
    }
}

/// A dataset snapshot may restore dataset-coupled thresholds, but it must never change which ASR
/// engine the operator is currently running or re-enable heavyweight background inference. Those
/// are live machine/runtime decisions, not historical dataset state.
pub(crate) fn preserve_live_asr_runtime_controls(restored: &mut AppSettings, live: &AppSettings) {
    restored.asr_model_size = live.asr_model_size.clone();
    restored.use_finetuned_asr = live.use_finetuned_asr;
    restored.multi_engine_hypotheses = live.multi_engine_hypotheses;
    restored.external_asr_script_path = live.external_asr_script_path.clone();
    restored.champion_supervision_enabled = live.champion_supervision_enabled;
}

pub(crate) fn restore_required_snapshot_state_atomic(
    plan: &[(crate::snapshot::OptionalSnapshotState, crate::snapshot::OptionalSnapshotRestore)],
    data_dir: &Path,
) -> Result<(), String> {
    for (state, action) in plan {
        if state.live_file == "settings.json" {
            continue;
        }
        let destination = data_dir.join(state.live_file);
        match action {
            crate::snapshot::OptionalSnapshotRestore::Install(bytes) => {
                atomic_write_restore_state(&destination, bytes).map_err(|error| {
                    format!("required snapshot state {} could not be installed atomically: {error}", state.live_file)
                })?;
            }
            crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent => {
                remove_live_restore_state(&destination).map_err(|error| {
                    format!("required snapshot state {} could not be made explicitly absent: {error}", state.live_file)
                })?;
            }
            crate::snapshot::OptionalSnapshotRestore::PreserveLegacy => {}
        }
    }
    Ok(())
}

pub(crate) fn apply_snapshot_pilot_policy(plan: &SnapshotPilotPolicyRestore, data_dir: &Path) -> Result<(), String> {
    let live = data_dir.join(crate::review_pilot::REVIEW_PILOT_FILE);
    match plan {
        SnapshotPilotPolicyRestore::Install(bytes) => atomic_write_restore_state(&live, bytes),
        SnapshotPilotPolicyRestore::ExplicitlyAbsent => remove_live_restore_state(&live)
            .map_err(|error| format!("could not apply explicit no-pilot snapshot state: {error}")),
        SnapshotPilotPolicyRestore::PreserveLegacy => Ok(()),
    }
}

pub(crate) fn strict_live_settings_for_restore(path: &Path) -> Result<AppSettings, String> {
    crate::atomic_file::recover_interrupted_replace(path)
        .map_err(|error| format!("could not recover live settings before restore: {error}"))?;
    let mut settings = match std::fs::read(path) {
        Ok(bytes) => crate::settings::AppSettings::parse_recovery_bytes(&bytes)
            .map_err(|error| format!("live settings are invalid; restore remains blocked: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::default(),
        Err(error) => return Err(format!("live settings are unreadable; restore remains blocked: {error}")),
    };
    settings.enforce_production_canon();
    Ok(settings)
}

pub(crate) fn install_snapshot_restore_plan(
    restore_plan: &SnapshotRestorePlan,
    data_dir: &Path,
    live_controls: &AppSettings,
) -> Result<AppSettings, String> {
    // Dataset-routing state must agree with the restored DB before paid review can resume. Install
    // every required file atomically; settings is typed and handled separately so historical cloud
    // consent and machine routing never touch live disk.
    restore_required_snapshot_state_atomic(&restore_plan.optional, data_dir)?;
    apply_snapshot_pilot_policy(&restore_plan.pilot, data_dir)?;

    let settings_action = restore_plan
        .optional
        .iter()
        .find(|(state, _)| state.live_file == "settings.json")
        .map(|(_, action)| action)
        .ok_or_else(|| "snapshot restore plan omitted settings state".to_string())?;
    let mut restored = match settings_action {
        crate::snapshot::OptionalSnapshotRestore::Install(bytes) => {
            crate::settings::AppSettings::parse_recovery_bytes(bytes)?
        }
        crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent => crate::settings::AppSettings::default(),
        crate::snapshot::OptionalSnapshotRestore::PreserveLegacy => live_controls.clone(),
    };
    // Consent and ASR/GPU controls are live operator decisions, never historical dataset state.
    restored.cloud_llm_opt_in = live_controls.cloud_llm_opt_in;
    restored.jury_cloud_opt_in = live_controls.jury_cloud_opt_in;
    preserve_live_asr_runtime_controls(&mut restored, live_controls);
    restored.enforce_production_canon();
    let live_settings_path = data_dir.join("settings.json");
    restored.save(&live_settings_path).map_err(|error| {
        format!(
            "snapshot database was restored, but live-control-preserving settings could not be installed; paid review remains blocked: {error}"
        )
    })?;
    crate::atomic_file::fsync_parent_dir_strict(&live_settings_path).map_err(|error| {
        format!(
            "snapshot settings were installed, but their directory metadata is not durably synchronized; paid review remains blocked: {error}"
        )
    })?;
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_segment(id: &str, transcript: &str) -> crate::db::SpeechSegment {
        crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: format!("C:/snapshot/{id}.wav"),
            raw_transcript: transcript.to_string(),
            ..Default::default()
        }
    }

    fn snapshot_fixture(
        parent: &Path,
        name: &str,
        segment_id: &str,
        transcript: &str,
        vad_threshold: f32,
        timestamp: u64,
    ) -> PathBuf {
        let data_dir = parent.join(name);
        std::fs::create_dir_all(&data_dir).unwrap();
        AppSettings { vad_threshold, ..AppSettings::default() }.save(&data_dir.join("settings.json")).unwrap();
        let database_path = data_dir.join("cortex-speech.db");
        let database = crate::db::Database::open(database_path.to_string_lossy().as_ref()).unwrap();
        database.initialize().unwrap();
        database.insert_segment(&restore_segment(segment_id, transcript)).unwrap();
        let snapshot = crate::snapshot::take_snapshot_at(&database, &data_dir, 5, timestamp).unwrap().unwrap();
        drop(database);
        snapshot
    }

    #[derive(Debug, Clone, Copy)]
    enum SnapshotSwap {
        WholeValidGeneration,
        DatabaseOnly,
        ConfigOnly,
        BytesAfterCaptureBeforeParse,
    }

    fn assert_snapshot_swap_refused_without_live_mutation(kind: SnapshotSwap) {
        let temp = tempfile::tempdir().unwrap();
        let selected = snapshot_fixture(temp.path(), "selected", "selected", "selected truth", 0.61, 1001);
        let replacement = snapshot_fixture(temp.path(), "replacement", "replacement", "replacement truth", 0.77, 2002);

        let live_dir = temp.path().join("live");
        std::fs::create_dir_all(&live_dir).unwrap();
        AppSettings { vad_threshold: 0.55, ..AppSettings::default() }.save(&live_dir.join("settings.json")).unwrap();
        let live_database_path = live_dir.join("cortex-speech.db");
        let live_database = crate::db::Database::open(live_database_path.to_string_lossy().as_ref()).unwrap();
        live_database.initialize().unwrap();
        live_database.insert_segment(&restore_segment("live", "live truth")).unwrap();
        drop(live_database);
        let live_database_before = std::fs::read(&live_database_path).unwrap();
        let live_settings_before = std::fs::read(live_dir.join("settings.json")).unwrap();

        let selected_db = selected.join("cortex-speech.db");
        let hook_selected = selected.clone();
        let hook_replacement = replacement.clone();
        let error = prepare_named_restore_artifacts(&selected, &selected_db, move || match kind {
            SnapshotSwap::WholeValidGeneration => {
                let displaced = hook_selected.with_file_name("snapshot_1001.displaced");
                std::fs::rename(&hook_selected, displaced).unwrap();
                std::fs::rename(&hook_replacement, &hook_selected).unwrap();
            }
            SnapshotSwap::DatabaseOnly => {
                std::fs::copy(hook_replacement.join("cortex-speech.db"), hook_selected.join("cortex-speech.db"))
                    .unwrap();
            }
            SnapshotSwap::ConfigOnly => {
                std::fs::copy(hook_replacement.join("settings.json"), hook_selected.join("settings.json")).unwrap();
            }
            SnapshotSwap::BytesAfterCaptureBeforeParse => {
                std::fs::write(hook_selected.join("settings.json"), b"tampered after exact-byte capture").unwrap();
            }
        })
        .err()
        .expect("a selected snapshot generation swap must fail closed");

        assert!(error.contains("generation digest mismatch"), "{kind:?}: {error}");
        assert_eq!(std::fs::read(&live_database_path).unwrap(), live_database_before, "{kind:?}: live DB changed");
        assert_eq!(
            std::fs::read(live_dir.join("settings.json")).unwrap(),
            live_settings_before,
            "{kind:?}: live settings changed"
        );
        let live = crate::db::Database::open(live_database_path.to_string_lossy().as_ref()).unwrap();
        assert!(live.get_segment_by_id("live").unwrap().is_some(), "{kind:?}: live truth disappeared");
        assert!(live.get_segment_by_id("selected").unwrap().is_none(), "{kind:?}: selected truth leaked live");
        assert!(live.get_segment_by_id("replacement").unwrap().is_none(), "{kind:?}: replacement truth leaked live");
    }

    #[test]
    fn immutable_snapshot_image_stages_one_exact_generation() {
        let temp = tempfile::tempdir().unwrap();
        let selected = snapshot_fixture(temp.path(), "selected", "selected", "selected truth", 0.61, 1001);
        let selected_db = selected.join("cortex-speech.db");
        let (plan, staged) = prepare_named_restore_artifacts(&selected, &selected_db, || {}).unwrap();

        assert!(staged.get_segment_by_id("selected").unwrap().is_some());
        let settings_action = plan
            .optional
            .iter()
            .find(|(state, _)| state.live_file == "settings.json")
            .map(|(_, action)| action)
            .unwrap();
        let crate::snapshot::OptionalSnapshotRestore::Install(bytes) = settings_action else {
            panic!("settings must be owned by the private snapshot plan");
        };
        let settings = AppSettings::parse_recovery_bytes(bytes).unwrap();
        assert_eq!(settings.vad_threshold, 0.61);
    }

    #[test]
    fn whole_valid_snapshot_generation_swap_is_refused_before_live_mutation() {
        assert_snapshot_swap_refused_without_live_mutation(SnapshotSwap::WholeValidGeneration);
    }

    #[test]
    fn database_only_snapshot_generation_swap_is_refused_before_live_mutation() {
        assert_snapshot_swap_refused_without_live_mutation(SnapshotSwap::DatabaseOnly);
    }

    #[test]
    fn config_only_snapshot_generation_swap_is_refused_before_live_mutation() {
        assert_snapshot_swap_refused_without_live_mutation(SnapshotSwap::ConfigOnly);
    }

    #[test]
    fn bytes_changed_after_capture_before_parse_are_refused_before_live_mutation() {
        assert_snapshot_swap_refused_without_live_mutation(SnapshotSwap::BytesAfterCaptureBeforeParse);
    }

    #[test]
    fn admission_is_published_before_the_writer_fence_and_released_on_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let admission = RestoreAdmission::new();
        let error = match prepare_restore_admission_with(directory.path().to_path_buf(), &admission, || {
            assert!(admission.is_pending(), "writer fence must observe the published reservation");
            true
        }) {
            Err(error) => error,
            Ok(_) => panic!("active writer must refuse restore admission"),
        };
        assert!(error.contains("background write is in progress"));
        assert!(!admission.is_pending(), "refusal must release an unarmed reservation");

        let (reservation, returned) =
            prepare_restore_admission_with(directory.path().to_path_buf(), &admission, || false).unwrap();
        assert_eq!(returned, directory.path());
        assert!(reservation.is_active());
        assert!(admission.is_pending());
        drop(reservation);
        assert!(!admission.is_pending());
    }

    #[test]
    fn durable_marker_round_trip_binds_one_target_and_completion_generation() {
        let directory = tempfile::tempdir().unwrap();
        let state = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: "rotating/snapshot-1".to_string(),
            pre_restore_pin_selector: "pinned/prerestore-1".to_string(),
            target_db_generation_sha256: Some("1".repeat(64)),
            completed_selector: None,
            completed_db_generation_sha256: None,
        };
        write_named_restore_pending(directory.path(), &state).unwrap();
        assert_eq!(load_named_restore_pending(directory.path()).unwrap(), Some(state.clone()));
        assert!(named_restore_barrier_may_exist(directory.path()));

        let error = mark_named_restore_completed(directory.path(), "rotating/snapshot-2", &"2".repeat(64)).unwrap_err();
        assert!(error.contains("not the recorded target"));
        mark_named_restore_completed(directory.path(), &state.source_selector, &"1".repeat(64)).unwrap();
        let completed = load_named_restore_pending(directory.path()).unwrap().unwrap();
        assert_eq!(completed.completed_selector.as_deref(), Some(state.source_selector.as_str()));
        assert_eq!(completed.completed_db_generation_sha256.as_deref(), Some("1".repeat(64).as_str()));

        clear_review_pilot_restore_pending(directory.path()).unwrap();
        assert_eq!(load_named_restore_pending(directory.path()).unwrap(), None);
        assert!(!named_restore_barrier_may_exist(directory.path()));
    }

    #[test]
    fn schema_three_restore_marker_rejects_missing_or_mismatched_generation_authority() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
        std::fs::write(
            &marker,
            br#"{
              "schema": 3,
              "sourceSelector": "snapshot_1",
              "preRestorePinSelector": "pinned/original_1"
            }"#,
        )
        .unwrap();
        let missing = load_named_restore_pending(directory.path()).unwrap_err();
        assert!(missing.contains("missing its target database generation"), "{missing}");

        std::fs::write(
            &marker,
            format!(
                "{{\n  \"schema\": 3,\n  \"sourceSelector\": \"snapshot_1\",\n  \
                 \"preRestorePinSelector\": \"pinned/original_1\",\n  \
                 \"targetDbGenerationSha256\": \"{}\",\n  \
                 \"completedSelector\": \"snapshot_1\",\n  \
                 \"completedDbGenerationSha256\": \"{}\"\n}}\n",
                "1".repeat(64),
                "2".repeat(64)
            ),
        )
        .unwrap();
        let mismatch = load_named_restore_pending(directory.path()).unwrap_err();
        assert!(mismatch.contains("different database generation"), "{mismatch}");
        assert!(named_restore_barrier_may_exist(directory.path()), "invalid authority must remain fail-closed");
    }

    #[test]
    fn snapshot_plan_restores_dataset_settings_but_preserves_live_machine_controls() {
        let directory = tempfile::tempdir().unwrap();
        let live = AppSettings {
            asr_model_size: crate::settings::AsrModelSize::WSL7B,
            use_finetuned_asr: false,
            multi_engine_hypotheses: false,
            external_asr_script_path: "C:/cortex/scripts/cortex_7b_client.py".to_string(),
            champion_supervision_enabled: false,
            ..AppSettings::default()
        };
        let historical = AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            use_finetuned_asr: true,
            multi_engine_hypotheses: true,
            external_asr_script_path: "historical-client.py".to_string(),
            champion_supervision_enabled: true,
            cloud_llm_opt_in: true,
            jury_cloud_opt_in: true,
            vad_threshold: 0.77,
            ..AppSettings::default()
        };
        let settings_state = crate::snapshot::OPTIONAL_SNAPSHOT_STATE
            .iter()
            .copied()
            .find(|state| state.live_file == "settings.json")
            .unwrap();
        let plan = SnapshotRestorePlan {
            pilot: SnapshotPilotPolicyRestore::ExplicitlyAbsent,
            optional: vec![(
                settings_state,
                crate::snapshot::OptionalSnapshotRestore::Install(serde_json::to_vec_pretty(&historical).unwrap()),
            )],
            expected_db_generation_sha256: "0".repeat(64),
        };

        let restored = install_snapshot_restore_plan(&plan, directory.path(), &live).unwrap();
        assert_eq!(restored.asr_model_size, live.asr_model_size);
        assert_eq!(restored.use_finetuned_asr, live.use_finetuned_asr);
        assert_eq!(restored.multi_engine_hypotheses, live.multi_engine_hypotheses);
        assert_eq!(restored.external_asr_script_path, live.external_asr_script_path);
        assert_eq!(restored.champion_supervision_enabled, live.champion_supervision_enabled);
        assert_eq!(restored.cloud_llm_opt_in, live.cloud_llm_opt_in);
        assert_eq!(restored.jury_cloud_opt_in, live.jury_cloud_opt_in);
        assert_eq!(restored.vad_threshold, historical.vad_threshold);
        let persisted = strict_live_settings_for_restore(&directory.path().join("settings.json")).unwrap();
        assert_eq!(persisted.asr_model_size, restored.asr_model_size);
        assert_eq!(persisted.external_asr_script_path, restored.external_asr_script_path);
        assert_eq!(persisted.champion_supervision_enabled, restored.champion_supervision_enabled);
        assert_eq!(persisted.cloud_llm_opt_in, restored.cloud_llm_opt_in);
        assert_eq!(persisted.jury_cloud_opt_in, restored.jury_cloud_opt_in);
        assert_eq!(persisted.vad_threshold, restored.vad_threshold);
    }
}
