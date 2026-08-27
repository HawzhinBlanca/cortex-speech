//! Settings / config / API-key IPC commands — slice 12 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use settings::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::get_settings` and the frontend invokes are
//! untouched. Same functions, only relocated.
//!
//! update_settings refreshes the live pipeline through AppState::update_pipeline_settings (never locking
//! the pipeline/settings directly) — byte-for-byte identical to the pre-decomposition commands.rs.
//! set_api_key persists via the DPAPI-protected key store (P0.3, 2026-07-24 — the write path was
//! upgraded from plaintext save_key to save_key_protected; see the note at that call site).

use super::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::ipc_contract::{
    ApiKeyProviderV1, CloudConsentKindV1, CommandErrorV1, RendererSettingsV1, SetCloudConsentRequestV1, SettingValueV1,
    SettingsPatchResultV1, SettingsPatchV1, SettingsSnapshotV1, SuggestedActionV1,
};
use crate::settings::AppSettings;
use crate::AppState;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tauri::State;

const MAX_SETTINGS_PATCH_FIELDS: usize = 64;
// JavaScript represents integers exactly only through 2^53 - 1. The contract deliberately keeps an
// i64 for Rust/SQLite interoperability, while this mask makes every opaque revision round-trip
// exactly through the generated TypeScript binding.
const SETTINGS_REVISION_MASK: u64 = (1_u64 << 53) - 1;

const PATCHABLE_SETTINGS_FIELDS: &[&str] = &[
    "vad_threshold",
    "min_segment_duration_ms",
    "max_segment_duration_ms",
    "num_asr_threads",
    "enable_gpu",
    "language",
    "export_format",
    "auto_normalize",
    "verbalize_numbers",
    "auto_align",
    "assign_speaker_from_filename",
    "enable_diarization",
    "enable_denoising",
    "autoplay_segments",
    "max_speakers",
    "max_wer_threshold",
    "max_cer_threshold",
    "enforce_quality_gates",
    "theme",
    "llm_mode",
    "llm_endpoint",
    "llm_system_prompt",
    "llm_model",
    "external_asr_script_path",
    "hf_train_ratio",
    "hf_val_ratio",
    "hf_test_ratio",
    "hf_split_seed",
    "hf_speaker_disjoint",
    "hf_license",
    "jury_model",
    "jury_provider",
    "jury_self_consistency_n",
    "jury_autonomy_level",
    "jury_t1_threshold",
];

enum EvaluatedSettingsChange {
    AlreadyApplied(AppSettings),
    Apply(AppSettings),
}

fn public_settings_error(code: &str, message: &str, retryable: bool) -> CommandErrorV1 {
    let error = CommandErrorV1::new(code, message, retryable);
    if retryable {
        error.suggested(SuggestedActionV1::Retry)
    } else {
        error
    }
}

fn public_api_key_error(code: &str, message: &str, retryable: bool) -> CommandErrorV1 {
    let error = CommandErrorV1::new(code, message, retryable);
    if retryable {
        error.suggested(SuggestedActionV1::Retry)
    } else {
        error.suggested(SuggestedActionV1::OpenHealth)
    }
}

fn validate_public_api_key(key: &str) -> Result<(), CommandErrorV1> {
    let trimmed = key.trim();
    if trimmed.chars().count() > crate::api_keys::MAX_API_KEY_CHARS
        || trimmed.contains(|character: char| character.is_control() || character.is_whitespace())
    {
        return Err(CommandErrorV1::new(
            "INVALID_API_KEY",
            "The API key is malformed or exceeds the supported length.",
            false,
        ));
    }
    Ok(())
}

fn settings_wire_bytes(settings: &AppSettings) -> Result<Vec<u8>, CommandErrorV1> {
    serde_json::to_vec(&settings.for_client_response()).map_err(|_| {
        public_settings_error(
            "SETTINGS_SNAPSHOT_FAILED",
            "The current settings could not be represented safely. Open Health before changing them.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })
}

fn settings_revision(settings: &AppSettings) -> Result<i64, CommandErrorV1> {
    let digest = Sha256::digest(settings_wire_bytes(settings)?);
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    Ok((u64::from_be_bytes(prefix) & SETTINGS_REVISION_MASK) as i64)
}

fn renderer_settings(settings: &AppSettings) -> Result<RendererSettingsV1, CommandErrorV1> {
    // Deserialize a strict renderer-safe subset from the same serde representation persisted by the
    // backend. Extra fields (API-key value and private internal paths) are intentionally ignored.
    let value = serde_json::to_value(settings.for_client_response()).map_err(|_| {
        public_settings_error(
            "SETTINGS_SNAPSHOT_FAILED",
            "The current settings could not be represented safely. Open Health before changing them.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })?;
    serde_json::from_value(value).map_err(|_| {
        public_settings_error(
            "SETTINGS_SNAPSHOT_FAILED",
            "The current settings do not match the public settings contract. Open Health before changing them.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })
}

fn settings_result(settings: &AppSettings, already_applied: bool) -> Result<SettingsPatchResultV1, CommandErrorV1> {
    Ok(SettingsPatchResultV1 {
        settings_revision: settings_revision(settings)?,
        settings: renderer_settings(settings)?,
        already_applied,
    })
}

fn invalid_patch(code: &str, message: &str) -> CommandErrorV1 {
    public_settings_error(code, message, false)
}

fn json_patch_value(
    current: &serde_json::Value,
    changed: &SettingValueV1,
) -> Result<serde_json::Value, CommandErrorV1> {
    match (current, changed) {
        (serde_json::Value::String(_), SettingValueV1::String(value)) => Ok(serde_json::Value::String(value.clone())),
        (serde_json::Value::Bool(_), SettingValueV1::Boolean(value)) => Ok(serde_json::Value::Bool(*value)),
        (serde_json::Value::Number(current_number), SettingValueV1::Number(value)) => {
            if !value.is_finite() {
                return Err(invalid_patch("INVALID_SETTINGS_VALUE", "A numeric setting was not finite."));
            }
            let number = if current_number.is_u64() {
                if *value < 0.0 || value.fract() != 0.0 || *value > u64::MAX as f64 {
                    return Err(invalid_patch(
                        "INVALID_SETTINGS_VALUE",
                        "An integer setting must be a non-negative whole number.",
                    ));
                }
                serde_json::Number::from(*value as u64)
            } else if current_number.is_i64() {
                if value.fract() != 0.0 || *value < i64::MIN as f64 || *value > i64::MAX as f64 {
                    return Err(invalid_patch(
                        "INVALID_SETTINGS_VALUE",
                        "An integer setting must be a whole number in range.",
                    ));
                }
                serde_json::Number::from(*value as i64)
            } else {
                serde_json::Number::from_f64(*value).ok_or_else(|| {
                    invalid_patch("INVALID_SETTINGS_VALUE", "A numeric setting could not be represented safely.")
                })?
            };
            Ok(serde_json::Value::Number(number))
        }
        _ => Err(invalid_patch("INVALID_SETTINGS_VALUE_TYPE", "A settings change had the wrong value type.")),
    }
}

fn patched_candidate(
    current: &AppSettings,
    changed_fields: &BTreeMap<String, SettingValueV1>,
) -> Result<AppSettings, CommandErrorV1> {
    if changed_fields.len() > MAX_SETTINGS_PATCH_FIELDS {
        return Err(invalid_patch("SETTINGS_PATCH_TOO_LARGE", "The settings change contained too many fields."));
    }

    let mut value = serde_json::to_value(current.for_client_response())
        .map_err(|_| invalid_patch("SETTINGS_SNAPSHOT_FAILED", "The current settings could not be read safely."))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid_patch("SETTINGS_SNAPSHOT_FAILED", "The current settings could not be read safely."))?;

    for (field, changed) in changed_fields {
        match field.as_str() {
            "cloud_llm_opt_in" | "jury_cloud_opt_in" => {
                return Err(invalid_patch(
                    "CONSENT_COMMAND_REQUIRED",
                    "Cloud consent must be changed through the explicit consent command.",
                ));
            }
            "llm_api_key" | "llm_api_key_configured" => {
                return Err(invalid_patch(
                    "SECRET_COMMAND_REQUIRED",
                    "API-key state must be changed through the explicit secret command.",
                ));
            }
            _ => {}
        }
        if !PATCHABLE_SETTINGS_FIELDS.contains(&field.as_str()) {
            return Err(invalid_patch(
                "SETTINGS_FIELD_NOT_PATCHABLE",
                "A requested field is not part of the public settings patch contract.",
            ));
        }
        let current_value = object
            .get(field)
            .ok_or_else(|| invalid_patch("SETTINGS_FIELD_NOT_FOUND", "A requested settings field is unavailable."))?;
        object.insert(field.clone(), json_patch_value(current_value, changed)?);
    }

    let mut next: AppSettings = serde_json::from_value(value)
        .map_err(|_| invalid_patch("INVALID_SETTINGS_PATCH", "One or more settings changes were invalid."))?;
    next.merge_session_secret_from(current);
    // As with the compatibility command, malicious selector input must fail before canonicalization;
    // canonicalization is only a migration for otherwise-valid legacy state.
    next.validate()
        .map_err(|_| invalid_patch("INVALID_SETTINGS_PATCH", "One or more settings changes failed validation."))?;
    next.enforce_production_canon();
    Ok(next)
}

fn stale_settings_error(expected: i64, current: i64) -> CommandErrorV1 {
    public_settings_error(
        "STALE_SETTINGS_REVISION",
        "Settings changed in another writer. Reload the authoritative settings before saving again.",
        false,
    )
    .detail("expectedSettingsRevision", expected)
    .detail("currentSettingsRevision", current)
}

fn evaluate_patch(current: &AppSettings, patch: &SettingsPatchV1) -> Result<EvaluatedSettingsChange, CommandErrorV1> {
    let current_revision = settings_revision(current)?;
    let next = patched_candidate(current, &patch.changed_fields)?;
    let same_effect = settings_wire_bytes(current)? == settings_wire_bytes(&next)?;
    if patch.expected_settings_revision != current_revision {
        if same_effect {
            // Exact response-loss replay (or an independently identical writer): the requested
            // effect is already the durable truth, so returning success cannot clobber anything.
            return Ok(EvaluatedSettingsChange::AlreadyApplied(current.clone()));
        }
        return Err(stale_settings_error(patch.expected_settings_revision, current_revision));
    }
    if same_effect {
        Ok(EvaluatedSettingsChange::AlreadyApplied(current.clone()))
    } else {
        Ok(EvaluatedSettingsChange::Apply(next))
    }
}

fn consent_candidate(current: &AppSettings, request: &SetCloudConsentRequestV1) -> AppSettings {
    let mut next = current.clone();
    match request.consent {
        CloudConsentKindV1::Llm => next.cloud_llm_opt_in = request.granted,
        CloudConsentKindV1::Jury => next.jury_cloud_opt_in = request.granted,
    }
    next
}

fn evaluate_consent(
    current: &AppSettings,
    request: &SetCloudConsentRequestV1,
) -> Result<EvaluatedSettingsChange, CommandErrorV1> {
    let current_revision = settings_revision(current)?;
    let next = consent_candidate(current, request);
    let same_effect = settings_wire_bytes(current)? == settings_wire_bytes(&next)?;
    if request.expected_settings_revision != current_revision {
        if same_effect {
            return Ok(EvaluatedSettingsChange::AlreadyApplied(current.clone()));
        }
        return Err(stale_settings_error(request.expected_settings_revision, current_revision));
    }
    if same_effect {
        Ok(EvaluatedSettingsChange::AlreadyApplied(current.clone()))
    } else {
        Ok(EvaluatedSettingsChange::Apply(next))
    }
}

fn persist_settings_change(
    state: &AppState,
    next: &AppSettings,
    withdraw_consent_before_save: bool,
) -> Result<(), CommandErrorV1> {
    let settings_path = state.lock_data_dir().clone().map(|dir| dir.join("settings.json"));
    if withdraw_consent_before_save {
        // A privacy withdrawal is a stop instruction. It reaches the running pipeline even if a
        // full/read-only disk prevents durable publication; this can only diverge in the safer OFF
        // direction, and the caller still receives the persistence failure.
        state.revoke_pipeline_consent_now(next);
    }
    if let Some(path) = settings_path {
        next.save(&path).map_err(|error| {
            tracing::error!("Failed to save revision-guarded settings: {error}");
            public_settings_error(
                "SETTINGS_PERSIST_FAILED",
                "Settings could not be saved to disk. No preference was published.",
                true,
            )
        })?;
    }
    *state.lock_settings() = next.clone();
    state.update_pipeline_settings(next.clone());
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    RATE_LIMITER.check("get_settings")?;
    let settings = state.lock_settings();
    Ok(settings.for_client_response())
}

#[tauri::command]
pub fn update_settings(mut settings: AppSettings, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_settings")?;
    // Settings is one leg of named snapshot recovery. Hold a full-operation mutation token so a
    // concurrent restore cannot interleave its restored settings/routing files with this save and
    // runtime pipeline update.
    let _mutation = super::begin_mutation()?;
    // Compatibility only: the generated renderer path uses patch_settings_v1. Still serialize this
    // legacy whole-object transaction with new clients so an older renderer cannot reorder the
    // pipeline publication after a newer revision-guarded write.
    let _settings_write = state.lock_settings_write();
    // Server-side trust boundary: reject a malicious endpoint/oversized payload before it
    // can take effect and redirect LLM requests (+ the API key) to an attacker's server. Validate
    // BEFORE canonicalization so a tampered webview cannot submit an unapproved cloud model and
    // receive a misleading success after the backend silently changes it. Legacy files are migrated
    // separately by AppSettings::load(); IPC input is an untrusted request and fails closed.
    settings.validate().map_err(|e| e.to_string())?;
    // The desktop never accepts a non-champion ASR route. Unlike the explicit cloud selectors above,
    // the frontend no longer exposes this legacy local selector, so canonicalizing it is a safe and
    // backwards-compatible migration for otherwise-valid older clients.
    settings.enforce_production_canon();
    // Round-22 #7: carry the in-session secret forward and capture the on-disk path WITHOUT yet
    // overwriting the in-memory copy. Persisting BEFORE committing to memory/pipeline means a save
    // failure (full/read-only/locked disk) leaves the in-memory settings, the running pipeline, AND
    // disk all consistent at the OLD value — never a three-way divergence where get_settings() reports
    // an unsaved change (including cloud-consent toggles) that the pipeline and the next launch ignore.
    let settings_path = {
        let current = state.lock_settings();
        settings.merge_session_secret_from(&current);
        state.lock_data_dir().clone().map(|d| d.join("settings.json"))
    };
    // WITHDRAWALS TAKE EFFECT BEFORE THE SAVE (external review 2026-08-06). Persist-first below is
    // right for a preference and for GRANTING consent, but a revocation must not be contingent on
    // free disk space: with a full or read-only disk the save fails, this function returns Err, and
    // without this line the running import would have kept uploading audio to the cloud with only a
    // save error to show for it.
    //
    // The residual divergence is deliberate and one-directional. If the save then fails, disk and
    // `get_settings()` still report the OLD (opted-in) value while egress is already stopped — the UI
    // can show cloud ON while nothing is being sent, never the reverse. Bounded to "safer than
    // displayed", and the returned error is what tells the user the next launch may not remember it.
    // `revocation_takes_effect_even_when_the_settings_save_fails` pins exactly that.
    state.revoke_pipeline_consent_now(&settings);
    // Persist FIRST, before committing to memory/pipeline. A save failure (e.g. a cloud-consent
    // toggle that never reached disk) must be SURFACED, not swallowed — otherwise the user believes
    // the change stuck while it silently reverts on the next launch (a privacy hazard for the cloud
    // opt-in toggles).
    if let Some(path) = settings_path {
        // Propagate a persist failure to the caller (the frontend surfaces settingsSaveFailed). On Err
        // we return BEFORE the commit below, so nothing observable changed: in-memory settings, the
        // running pipeline, AND disk all stay consistent at the OLD value — never a divergence where
        // get_settings() reports an unsaved change the pipeline and next launch ignore.
        settings.save(&path).map_err(|e| {
            tracing::error!("Failed to save settings to {path:?}: {e}");
            format!("Failed to save settings to disk: {e}")
        })?;
    }
    // Disk now holds the new value (or there is no data dir to persist to): commit it to the in-memory
    // store and the running pipeline together.
    *state.lock_settings() = settings.clone();
    state.update_pipeline_settings(settings);
    Ok(())
}

/// Read one authoritative settings snapshot and its opaque compare-and-swap revision atomically.
/// API-key values and private app-owned paths are absent from the generated renderer contract.
#[tauri::command]
#[specta::specta]
pub fn get_settings_v1(state: State<'_, AppState>) -> Result<SettingsSnapshotV1, CommandErrorV1> {
    RATE_LIMITER
        .check("get_settings_v1")
        .map_err(|_| public_settings_error("RATE_LIMITED", "Too many settings requests. Retry in a moment.", true))?;
    let current = state.lock_settings().clone();
    Ok(SettingsSnapshotV1 { settings_revision: settings_revision(&current)?, settings: renderer_settings(&current)? })
}

/// Apply only explicitly changed preference fields against the exact snapshot revision. A stale
/// writer cannot replace unrelated newer state; an exact response-loss replay succeeds only when
/// its complete requested effect is already authoritative.
#[tauri::command]
#[specta::specta]
pub fn patch_settings_v1(
    patch: SettingsPatchV1,
    state: State<'_, AppState>,
) -> Result<SettingsPatchResultV1, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("patch_settings_v1")
        .map_err(|_| public_settings_error("RATE_LIMITED", "Too many settings changes. Retry in a moment.", true))?;
    let _mutation = super::begin_mutation().map_err(|_| {
        public_settings_error(
            "SETTINGS_TEMPORARILY_UNAVAILABLE",
            "Settings cannot change while workspace recovery is active.",
            true,
        )
    })?;
    let _settings_write = state.lock_settings_write();
    let current = state.lock_settings().clone();
    match evaluate_patch(&current, &patch)? {
        EvaluatedSettingsChange::AlreadyApplied(settings) => settings_result(&settings, true),
        EvaluatedSettingsChange::Apply(next) => {
            // Construct the response before publication so a serialization bug cannot save the
            // change and then falsely report failure.
            let result = settings_result(&next, false)?;
            persist_settings_change(&state, &next, false)?;
            Ok(result)
        }
    }
}

/// Change one cloud permission as an explicit privacy transaction. Withdrawals stop live egress
/// before persistence; grants become effective only after durable settings publication.
#[tauri::command]
#[specta::specta]
pub fn set_cloud_consent_v1(
    request: SetCloudConsentRequestV1,
    state: State<'_, AppState>,
) -> Result<SettingsPatchResultV1, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("set_cloud_consent_v1")
        .map_err(|_| public_settings_error("RATE_LIMITED", "Too many consent changes. Retry in a moment.", true))?;
    let _mutation = super::begin_mutation().map_err(|_| {
        public_settings_error(
            "SETTINGS_TEMPORARILY_UNAVAILABLE",
            "Consent cannot change while workspace recovery is active.",
            true,
        )
    })?;
    let _settings_write = state.lock_settings_write();
    let current = state.lock_settings().clone();
    match evaluate_consent(&current, &request)? {
        EvaluatedSettingsChange::AlreadyApplied(settings) => settings_result(&settings, true),
        EvaluatedSettingsChange::Apply(next) => {
            let result = settings_result(&next, false)?;
            persist_settings_change(&state, &next, !request.granted)?;
            Ok(result)
        }
    }
}

/// Report which cloud providers have an API key configured (provider NAMES only — never the key
/// values), so the user can confirm the keys they pasted into secrets.env were detected.
#[tauri::command]
#[specta::specta]
pub fn get_configured_providers(state: State<'_, AppState>) -> Result<Vec<String>, CommandErrorV1> {
    RATE_LIMITER
        .check("get_configured_providers")
        .map_err(|_| public_api_key_error("RATE_LIMITED", "The API-key status is busy. Retry in a moment.", true))?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| {
        public_api_key_error(
            "API_KEY_STORE_UNAVAILABLE",
            "The local API-key store is unavailable. Open Health for recovery options.",
            false,
        )
    })?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir).map_err(|_| {
        public_api_key_error(
            "API_KEY_STATUS_FAILED",
            "The configured API-key status could not be read safely. Open Health for recovery options.",
            false,
        )
    })?;
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}

/// Save one provider API key into `secrets.env` from the Settings UI (an empty key clears it).
/// The key value goes straight to the local secrets file — it is never logged, never echoed back, and
/// never stored in settings.json/DB. Returns the configured provider NAMES so the UI can refresh its
/// set/unset badges without ever seeing the value again.
#[tauri::command]
#[specta::specta]
pub fn set_api_key(
    provider: ApiKeyProviderV1,
    key: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("set_api_key")
        .map_err(|_| public_api_key_error("RATE_LIMITED", "Too many API-key changes. Retry in a moment.", true))?;
    validate_public_api_key(&key)?;
    let name = match provider {
        ApiKeyProviderV1::Gemini => "GEMINI_API_KEY",
        ApiKeyProviderV1::Openrouter => "OPENROUTER_API_KEY",
    };
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| {
        public_api_key_error(
            "API_KEY_STORE_UNAVAILABLE",
            "The local API-key store is unavailable. Open Health for recovery options.",
            false,
        )
    })?;
    // P0.3 (2026-07-24 audit H4): DPAPI-encrypt the key at rest on Windows (the ship target) rather than
    // storing it plaintext — save_key_protected was built + unit-tested but never wired to production, so
    // the module header above (and the "privacy-first" posture) was capability theater. On non-Windows a
    // non-empty key ERRORS instead of silently storing plaintext under a "protected" API (the honest
    // fail-safe); clearing a key (empty value) still works everywhere. Existing plaintext keys keep
    // loading (parse_env_file reads both) and upgrade to a dpapi: blob the next time they are saved here.
    crate::api_keys::ApiKeys::save_key_protected(&data_dir, name, &key).map_err(|_| {
        public_api_key_error(
            "API_KEY_SAVE_FAILED",
            "The API key could not be saved safely. Open Health for recovery options.",
            false,
        )
    })?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir).map_err(|_| {
        public_api_key_error(
            "API_KEY_STATUS_FAILED",
            "The key was saved, but its configured status could not be reread safely. Open Health before retrying.",
            false,
        )
    })?;
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_ipc_is_closed_bounded_and_never_echoes_secret_material() {
        assert_eq!(serde_json::to_value(ApiKeyProviderV1::Gemini).unwrap(), "gemini");
        assert_eq!(serde_json::to_value(ApiKeyProviderV1::Openrouter).unwrap(), "openrouter");

        let hostile = format!("token=secret\n{}", "x".repeat(crate::api_keys::MAX_API_KEY_CHARS + 1));
        let error = validate_public_api_key(&hostile).expect_err("hostile secret must be refused");
        let wire = serde_json::to_string(&error).expect("serialize public API-key error");
        assert!(wire.contains("INVALID_API_KEY"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("token"));
        assert!(!wire.contains(&hostile));

        let storage = public_api_key_error(
            "API_KEY_SAVE_FAILED",
            "The API key could not be saved safely. Open Health for recovery options.",
            false,
        );
        let storage_wire = serde_json::to_string(&storage).unwrap();
        assert!(!storage_wire.contains("C:\\"));
        assert!(!storage_wire.contains("SQL"));
    }

    fn patch(revision: i64, field: &str, value: SettingValueV1) -> SettingsPatchV1 {
        SettingsPatchV1 {
            expected_settings_revision: revision,
            changed_fields: BTreeMap::from([(field.to_string(), value)]),
        }
    }

    fn applied(change: EvaluatedSettingsChange) -> AppSettings {
        match change {
            EvaluatedSettingsChange::Apply(settings) => settings,
            EvaluatedSettingsChange::AlreadyApplied(_) => panic!("expected a new settings effect"),
        }
    }

    #[test]
    fn stale_concurrent_settings_writer_cannot_clobber_the_first_writer() {
        let base = AppSettings::default();
        let base_revision = settings_revision(&base).expect("base revision");
        let first = patch(base_revision, "autoplay_segments", SettingValueV1::Boolean(true));
        let second = patch(base_revision, "verbalize_numbers", SettingValueV1::Boolean(false));

        let after_first = applied(evaluate_patch(&base, &first).expect("first writer applies"));
        let refusal = match evaluate_patch(&after_first, &second) {
            Err(error) => error,
            Ok(_) => panic!("a stale second writer must not apply"),
        };

        assert_eq!(refusal.code, "STALE_SETTINGS_REVISION");
        assert!(after_first.autoplay_segments, "the first writer remains authoritative");
        assert!(after_first.verbalize_numbers, "the stale writer changed nothing");
    }

    #[test]
    fn exact_patch_replay_after_a_lost_response_is_idempotent() {
        let base = AppSettings::default();
        let request =
            patch(settings_revision(&base).expect("base revision"), "autoplay_segments", SettingValueV1::Boolean(true));
        let committed = applied(evaluate_patch(&base, &request).expect("initial application"));

        let replay = evaluate_patch(&committed, &request).expect("exact response-loss replay");
        let authoritative = match replay {
            EvaluatedSettingsChange::AlreadyApplied(settings) => settings,
            EvaluatedSettingsChange::Apply(_) => panic!("an exact replay must not create a second effect"),
        };
        assert!(authoritative.autoplay_segments);
        assert_eq!(
            settings_revision(&authoritative).unwrap(),
            settings_revision(&committed).unwrap(),
            "replay returns the existing authoritative revision"
        );
    }

    #[test]
    fn exact_consent_replay_is_idempotent_but_a_conflicting_stale_writer_is_refused() {
        let base = AppSettings::default();
        let revision = settings_revision(&base).unwrap();
        let grant = SetCloudConsentRequestV1 {
            expected_settings_revision: revision,
            consent: CloudConsentKindV1::Llm,
            granted: true,
        };
        let granted = applied(evaluate_consent(&base, &grant).expect("grant applies"));
        assert!(matches!(
            evaluate_consent(&granted, &grant).expect("grant replay"),
            EvaluatedSettingsChange::AlreadyApplied(_)
        ));

        let stale_jury_grant = SetCloudConsentRequestV1 {
            expected_settings_revision: revision,
            consent: CloudConsentKindV1::Jury,
            granted: true,
        };
        let refusal = match evaluate_consent(&granted, &stale_jury_grant) {
            Err(error) => error,
            Ok(_) => panic!("a conflicting stale consent writer must not apply"),
        };
        assert_eq!(refusal.code, "STALE_SETTINGS_REVISION");
        assert!(!granted.jury_cloud_opt_in);
    }

    #[test]
    fn generic_patch_cannot_smuggle_consent_or_secret_state() {
        let current = AppSettings::default();
        let revision = settings_revision(&current).unwrap();
        for (field, expected_code) in [
            ("cloud_llm_opt_in", "CONSENT_COMMAND_REQUIRED"),
            ("jury_cloud_opt_in", "CONSENT_COMMAND_REQUIRED"),
            ("llm_api_key", "SECRET_COMMAND_REQUIRED"),
            ("llm_api_key_configured", "SECRET_COMMAND_REQUIRED"),
        ] {
            let request = patch(revision, field, SettingValueV1::Boolean(true));
            let error = match evaluate_patch(&current, &request) {
                Err(error) => error,
                Ok(_) => panic!("{field} must be rejected by the generic patch"),
            };
            assert_eq!(error.code, expected_code, "field {field}");
        }
    }

    #[test]
    fn integer_settings_reject_fractional_numbers_instead_of_rounding() {
        let current = AppSettings::default();
        let request = patch(settings_revision(&current).unwrap(), "num_asr_threads", SettingValueV1::Number(3.5));
        let error = match evaluate_patch(&current, &request) {
            Err(error) => error,
            Ok(_) => panic!("fractional integer setting must fail closed"),
        };
        assert_eq!(error.code, "INVALID_SETTINGS_VALUE");
    }

    #[test]
    fn revision_ignores_session_secret_and_renderer_snapshot_omits_secret_and_internal_paths() {
        let base = AppSettings { llm_api_key_configured: true, ..AppSettings::default() };
        let mut with_secret = base.clone();
        with_secret.llm_api_key = "never-cross-ipc".to_string();
        assert_eq!(settings_revision(&base).unwrap(), settings_revision(&with_secret).unwrap());

        let public = renderer_settings(&with_secret).expect("renderer settings");
        let json = serde_json::to_value(public).unwrap();
        assert!(json.get("llm_api_key").is_none());
        assert!(json.get("model_dir").is_none());
        assert!(json.get("output_dir").is_none());
        assert!(json.to_string().find("never-cross-ipc").is_none());
    }

    #[test]
    fn update_settings_rejects_noncanonical_cloud_routes_before_canonicalization_and_persistence() {
        let src = include_str!("settings.rs");
        let prod = src.split("mod tests").next().unwrap_or(src);
        let validate = prod.find("settings.validate()").expect("settings validation");
        let clamp = prod.find("settings.enforce_production_canon()").expect("production routing clamp");
        let save = prod.find("settings.save(&path)").expect("settings persistence");
        assert!(
            validate < clamp && clamp < save,
            "untrusted cloud selectors must fail validation before the champion clamp and persistence"
        );
    }

    /// P0.3 (audit H4): the sole production key-writer MUST DPAPI-encrypt at rest, never fall back to the
    /// plaintext `save_key`. Wiring `set_api_key` to `save_key` (the plaintext path) is the exact
    /// regression this guards; a source-invariant check because the command needs full AppState to call.
    /// Fail-before: the pre-fix line `ApiKeys::save_key(&data_dir, ...)` trips the second assertion.
    /// A withdrawal must reach the running pipeline BEFORE the save that can fail (external review
    /// 2026-08-06). The pipeline_tests unit tests prove `revoke_consent_now` behaves correctly; only
    /// this pins that `update_settings` actually CALLS it, and calls it before `save`.
    ///
    /// Fail-before verified BOTH ways: deleting the call, and moving it after the save. Neither broke
    /// compilation and neither was caught by any other gate — which is exactly why the ordering needs
    /// a source-shape guard rather than trusting the comment above it.
    #[test]
    fn a_consent_withdrawal_reaches_the_pipeline_before_the_save_that_may_fail() {
        let src = include_str!("settings.rs");
        let prod = src.split("mod tests").next().unwrap_or(src);
        let revoke = prod.find("state.revoke_pipeline_consent_now(&settings)");
        let save = prod.find("settings.save(&path)");
        let (revoke, save) = match (revoke, save) {
            (Some(r), Some(s)) => (r, s),
            _ => panic!(
                "update_settings must call revoke_pipeline_consent_now and settings.save — a missing \
                 revocation lets a full or read-only disk keep cloud egress running after the user \
                 switched it off"
            ),
        };
        assert!(
            revoke < save,
            "the withdrawal must be applied BEFORE the persist: after it, a failed save returns Err \
             and the running import keeps uploading"
        );
    }

    #[test]
    fn set_api_key_persists_via_dpapi_protected_store_not_plaintext() {
        // Scan ONLY the production region — everything before `mod tests` — so this assertion's own
        // string literals (which necessarily contain the forbidden call) don't match themselves.
        let src = include_str!("settings.rs");
        let prod = src.split("mod tests").next().unwrap_or(src);
        assert!(
            prod.contains("ApiKeys::save_key_protected(&data_dir"),
            "set_api_key must persist keys via the DPAPI-protected store (save_key_protected)"
        );
        assert!(
            !prod.contains("ApiKeys::save_key(&data_dir"),
            "set_api_key must NOT write API keys in plaintext via save_key(&data_dir, ...)"
        );
    }
}
