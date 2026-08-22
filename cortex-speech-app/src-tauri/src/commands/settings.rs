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
use crate::settings::AppSettings;
use crate::AppState;
use tauri::State;

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

/// Report which cloud providers have an API key configured (provider NAMES only — never the key
/// values), so the user can confirm the keys they pasted into secrets.env were detected.
#[tauri::command]
pub fn get_configured_providers(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    RATE_LIMITER.check("get_configured_providers")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir);
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}

/// Save one provider API key into `secrets.env` from the Settings UI (an empty key clears it).
/// The key value goes straight to the local secrets file — it is never logged, never echoed back, and
/// never stored in settings.json/DB. Returns the configured provider NAMES so the UI can refresh its
/// set/unset badges without ever seeing the value again.
#[tauri::command]
pub fn set_api_key(provider: String, key: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    STRICT_RATE_LIMITER.check("set_api_key")?;
    let name = match provider.as_str() {
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        other => return Err(format!("unknown provider '{other}'")),
    };
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    // P0.3 (2026-07-24 audit H4): DPAPI-encrypt the key at rest on Windows (the ship target) rather than
    // storing it plaintext — save_key_protected was built + unit-tested but never wired to production, so
    // the module header above (and the "privacy-first" posture) was capability theater. On non-Windows a
    // non-empty key ERRORS instead of silently storing plaintext under a "protected" API (the honest
    // fail-safe); clearing a key (empty value) still works everywhere. Existing plaintext keys keep
    // loading (parse_env_file reads both) and upgrade to a dpapi: blob the next time they are saved here.
    crate::api_keys::ApiKeys::save_key_protected(&data_dir, name, &key)?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir);
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}

#[cfg(test)]
mod tests {
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
