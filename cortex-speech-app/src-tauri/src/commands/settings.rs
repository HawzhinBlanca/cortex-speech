//! Settings / config / API-key IPC commands — slice 12 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use settings::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::get_settings` and the frontend invokes are
//! untouched. Same functions, only relocated.
//!
//! update_settings refreshes the live pipeline through AppState::update_pipeline_settings (never locking
//! the pipeline/settings directly) and set_api_key persists via the DPAPI-protected key store — both
//! byte-for-byte identical to before.

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
    // Server-side trust boundary: reject a malicious endpoint/oversized payload before it
    // can take effect and redirect LLM requests (+ the API key) to an attacker's server.
    settings.validate().map_err(|e| e.to_string())?;
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
        "elevenlabs" => "ELEVENLABS_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        other => return Err(format!("unknown provider '{other}'")),
    };
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    crate::api_keys::ApiKeys::save_key(&data_dir, name, &key)?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir);
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}
