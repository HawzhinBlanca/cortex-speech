//! Dependency-clean restore authority, staged-generation validation, and publication orchestration.
//!
//! This module deliberately has no Tauri or `AppState` dependency. Desktop commands adapt UI state
//! into these functions; SQL authority and cross-generation publication remain testable without a
//! renderer or runtime.

mod authority;
mod compensation;
mod effects;
mod orchestration;
mod pilot;
mod playback;

#[cfg(test)]
pub(crate) use authority::{
    has_durable_review_activity, require_consent_revocation_superset, require_durable_review_history_superset,
};
#[cfg(test)]
pub(crate) use compensation::validate_review_compensation_semantics;
#[cfg(test)]
pub(crate) use effects::validate_review_effect_semantics;
#[cfg(test)]
pub(crate) use orchestration::recover_interrupted_named_restore_with_admission;
pub(crate) use orchestration::{
    prepare_and_restore_named_transaction, recover_interrupted_named_restore_at_startup,
    restore_with_mandatory_snapshot,
};
#[cfg(test)]
pub(crate) use pilot::{require_active_pilot_policy_binding, validate_active_pilot_semantics};
#[cfg(test)]
pub(crate) use playback::{validate_playback_receipt_semantics, validate_restore_target_semantics};
