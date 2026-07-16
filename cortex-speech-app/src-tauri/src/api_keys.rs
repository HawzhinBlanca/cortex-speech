//! Local API-key store for the cloud engines (Gemini, ElevenLabs Scribe, OpenRouter).
//!
//! Keys live ONLY on the user's machine: read from `{app_data_dir}/secrets.env` (`KEY=VALUE` lines)
//! or, as an override, environment variables. They are NEVER logged (the `Debug` impl masks values),
//! NEVER committed (the file is in the app data dir, outside the repo), and never leave the device
//! except in the outbound request to each provider's own API. Cortex reads them here; nothing else
//! handles them in plaintext.

use std::collections::HashMap;
use std::path::Path;

/// The file, inside the app data dir, the user pastes their keys into.
pub const SECRETS_FILE: &str = "secrets.env";

/// The keys Cortex looks for.
pub const KEY_NAMES: [&str; 3] = ["GEMINI_API_KEY", "ELEVENLABS_API_KEY", "OPENROUTER_API_KEY"];

/// API keys, loaded from the local secrets file / environment. Optional — a provider with no key is
/// simply not used.
#[derive(Clone, Default)]
pub struct ApiKeys {
    pub gemini: Option<String>,
    pub elevenlabs: Option<String>,
    pub openrouter: Option<String>,
}

impl std::fmt::Debug for ApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER print key values — only whether each is set.
        let mark = |o: &Option<String>| if o.is_some() { "set" } else { "unset" };
        f.debug_struct("ApiKeys")
            .field("gemini", &mark(&self.gemini))
            .field("elevenlabs", &mark(&self.elevenlabs))
            .field("openrouter", &mark(&self.openrouter))
            .finish()
    }
}

impl ApiKeys {
    /// Load keys from `{data_dir}/secrets.env`, with environment variables taking precedence. A
    /// missing file yields all-`None` (never an error). Blank values count as unset, so a template
    /// line like `GEMINI_API_KEY=` does not register a key.
    pub fn load(data_dir: &Path) -> Self {
        let mut map = parse_env_file(&data_dir.join(SECRETS_FILE));
        for name in KEY_NAMES {
            if let Ok(value) = std::env::var(name) {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    map.remove(name);
                } else {
                    map.insert(name.to_string(), trimmed.to_string());
                }
            }
        }
        Self {
            gemini: map.remove("GEMINI_API_KEY"),
            elevenlabs: map.remove("ELEVENLABS_API_KEY"),
            openrouter: map.remove("OPENROUTER_API_KEY"),
        }
    }

    /// The provider names that have a key configured — for an honest "what's connected" status. This
    /// returns names only, NEVER the key values.
    pub fn configured_providers(&self) -> Vec<&'static str> {
        let mut providers = Vec::new();
        if self.gemini.is_some() {
            providers.push("gemini");
        }
        if self.elevenlabs.is_some() {
            providers.push("elevenlabs");
        }
        if self.openrouter.is_some() {
            providers.push("openrouter");
        }
        providers
    }

    /// Save (or clear, with an empty `value`) one API key in `{data_dir}/secrets.env`, preserving every
    /// other line (comments, other keys). Written atomically (tmp + rename). The value is validated
    /// against control characters/whitespace so a pasted value can never inject extra `KEY=` lines,
    /// and it is NEVER logged or echoed back.
    pub fn save_key(data_dir: &Path, name: &str, value: &str) -> Result<(), String> {
        let value = validate_key_value(name, value)?;
        // Empty clears the key (write a bare "NAME=" placeholder). A non-empty key is stored AS-IS
        // (plaintext); use save_key_protected for DPAPI at-rest protection.
        Self::write_key_line(data_dir, name, value)
    }

    /// Like [`save_key`], but DPAPI-encrypts the value at rest (`NAME=dpapi:<base64>`), tying it to the
    /// current Windows account. Empty value clears the key. Windows-only for a non-empty value; on
    /// other platforms it errors rather than silently storing plaintext under a protected API.
    pub fn save_key_protected(data_dir: &Path, name: &str, value: &str) -> Result<(), String> {
        let value = validate_key_value(name, value)?;
        if value.is_empty() {
            return Self::write_key_line(data_dir, name, String::new());
        }
        let protected = crate::dpapi::protect(&value)?;
        Self::write_key_line(data_dir, name, protected)
    }

    /// Shared writer: replace/insert `NAME=<stored>` in secrets.env atomically, preserving all other
    /// lines. `stored` is written verbatim (already plaintext-validated or a dpapi blob).
    fn write_key_line(data_dir: &Path, name: &str, stored: String) -> Result<(), String> {
        let path = data_dir.join(SECRETS_FILE);
        let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| Self::template());
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        let prefix = format!("{name}=");
        let new_line = format!("{name}={stored}"); // empty -> "NAME=" (a cleared, unset key)
        match lines.iter_mut().find(|l| l.trim_start().starts_with(&prefix)) {
            Some(line) => *line = new_line,
            None => lines.push(new_line),
        }
        let mut contents = lines.join("\n");
        contents.push('\n');

        std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
        let tmp = data_dir.join(format!("{SECRETS_FILE}.tmp"));
        std::fs::write(&tmp, &contents).map_err(|e| format!("write secrets temp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("replace secrets file: {e}"))?;
        Ok(())
    }

    /// The exact `secrets.env` template to drop next to the app's settings — empty placeholders the
    /// user fills in locally.
    pub fn template() -> String {
        "# Cortex API keys — LOCAL ONLY. Paste each key after the = sign, then save this file.\n\
         # This file stays on your machine. Keys are sent ONLY to each provider's API, are never\n\
         # logged, and are never committed to git. Leave a line blank to disable that provider.\n\
         \n\
         GEMINI_API_KEY=\n\
         ELEVENLABS_API_KEY=\n\
         OPENROUTER_API_KEY=\n"
            .to_string()
    }
}

/// Validate an API-key name + value before storing. Returns the trimmed value (possibly empty, which
/// means "clear this key"). Rejects unknown names and whitespace/control chars (a newline would inject
/// a second `KEY=` line; other whitespace/control chars are never part of a real provider key) — the
/// same guard for both plaintext and pre-DPAPI values.
fn validate_key_value(name: &str, value: &str) -> Result<String, String> {
    if !KEY_NAMES.contains(&name) {
        return Err(format!("unknown API key name '{name}'"));
    }
    let value = value.trim();
    if value.contains(|c: char| c.is_control() || c.is_whitespace()) {
        return Err("API key contains whitespace or control characters — paste the key exactly".to_string());
    }
    Ok(value.to_string())
}

/// Parse a minimal `.env` file: `KEY=VALUE` per line; `#` comments and blank lines ignored;
/// surrounding whitespace and a matching pair of single/double quotes stripped; blank values dropped.
fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        let bytes = value.as_bytes();
        if bytes.len() >= 2 {
            let q = bytes[0];
            if (q == b'"' || q == b'\'') && bytes[bytes.len() - 1] == q {
                value = &value[1..value.len() - 1];
            }
        }
        if !key.is_empty() && !value.is_empty() {
            // DPAPI-protected values (`dpapi:<base64>`) are transparently decrypted here so the rest of
            // the app only ever sees plaintext. A blob that can't be decrypted (copied to another
            // Windows account, or moved to a non-Windows box) is treated as UNSET — never surfaced as
            // the literal ciphertext — and logged so the failure is visible, not silent.
            if crate::dpapi::is_protected(value) {
                match crate::dpapi::unprotect(value) {
                    Ok(plain) if !plain.is_empty() => {
                        map.insert(key.to_string(), plain);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("could not decrypt DPAPI-protected {key} (treated as unset): {e}"),
                }
            } else {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_file_handles_comments_quotes_and_blanks() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.env");
        std::fs::write(
            &p,
            "# my keys\nGEMINI_API_KEY = abc123 \n\nELEVENLABS_API_KEY=\"xyz\"\nEMPTY=\n# OPENROUTER_API_KEY=nope\n",
        )
        .unwrap();
        let m = parse_env_file(&p);
        assert_eq!(m.get("GEMINI_API_KEY").map(String::as_str), Some("abc123"), "whitespace trimmed");
        assert_eq!(m.get("ELEVENLABS_API_KEY").map(String::as_str), Some("xyz"), "quotes stripped");
        assert!(!m.contains_key("EMPTY"), "blank value dropped");
        assert!(!m.contains_key("OPENROUTER_API_KEY"), "commented-out line not parsed");
    }

    #[test]
    fn parse_env_file_missing_is_empty_no_error() {
        assert!(parse_env_file(Path::new("/no/such/secrets.env")).is_empty());
    }

    #[test]
    fn configured_providers_lists_only_set_keys_never_values() {
        let keys = ApiKeys { gemini: Some("g".into()), elevenlabs: None, openrouter: Some("o".into()) };
        assert_eq!(keys.configured_providers(), vec!["gemini", "openrouter"]);
    }

    #[test]
    fn debug_never_leaks_key_values() {
        let keys = ApiKeys { gemini: Some("super-secret-key".into()), elevenlabs: None, openrouter: None };
        let rendered = format!("{keys:?}");
        assert!(!rendered.contains("super-secret-key"), "Debug must never print a key value");
        assert!(rendered.contains("set") && rendered.contains("unset"));
    }

    #[test]
    fn template_has_all_three_empty_placeholders() {
        let t = ApiKeys::template();
        for name in KEY_NAMES {
            assert!(t.contains(&format!("{name}=")), "template must include {name}");
        }
    }

    #[test]
    fn save_key_creates_updates_clears_and_preserves_other_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // First save on a machine with no secrets.env: file is created from the template.
        ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "sk-or-abc123").unwrap();
        let keys = ApiKeys::load(dir);
        assert_eq!(keys.openrouter.as_deref(), Some("sk-or-abc123"));
        let contents = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert!(contents.contains("GEMINI_API_KEY="), "template placeholders preserved");
        assert!(contents.starts_with('#'), "template comment preserved");

        // Update in place — other keys/lines untouched.
        ApiKeys::save_key(dir, "GEMINI_API_KEY", "g-key").unwrap();
        ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "sk-or-NEW").unwrap();
        let keys = ApiKeys::load(dir);
        assert_eq!(keys.openrouter.as_deref(), Some("sk-or-NEW"));
        assert_eq!(keys.gemini.as_deref(), Some("g-key"));
        let contents = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert_eq!(contents.matches("OPENROUTER_API_KEY=").count(), 1, "one line per key, replaced not appended");

        // Empty value clears the key (line stays as an unset placeholder).
        ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "").unwrap();
        assert!(ApiKeys::load(dir).openrouter.is_none(), "cleared key must read back as unset");
    }

    #[cfg(windows)]
    #[test]
    fn save_key_protected_encrypts_at_rest_and_loads_transparently() {
        // Week-2 credentials-off-plaintext: a DPAPI-protected save must (1) store a dpapi:<base64> blob
        // NOT the plaintext, and (2) load back transparently as plaintext. Plaintext keys still work.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let secret = "sk-or-super-secret-value";
        ApiKeys::save_key_protected(dir, "OPENROUTER_API_KEY", secret).unwrap();

        let on_disk = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert!(!on_disk.contains(secret), "the plaintext key must never be on disk");
        assert!(on_disk.contains("OPENROUTER_API_KEY=dpapi:"), "value stored as a DPAPI blob: {on_disk}");

        assert_eq!(ApiKeys::load(dir).openrouter.as_deref(), Some(secret), "loads back transparently");

        // A plaintext key alongside the protected one still loads (additive, not a replacement).
        ApiKeys::save_key(dir, "GEMINI_API_KEY", "g-plain").unwrap();
        let keys = ApiKeys::load(dir);
        assert_eq!(keys.gemini.as_deref(), Some("g-plain"));
        assert_eq!(keys.openrouter.as_deref(), Some(secret));

        // Clearing a protected key works through the same path.
        ApiKeys::save_key_protected(dir, "OPENROUTER_API_KEY", "").unwrap();
        assert!(ApiKeys::load(dir).openrouter.is_none());
    }

    #[test]
    fn a_corrupt_dpapi_blob_reads_as_unset_never_as_ciphertext() {
        // A dpapi: value that can't be decrypted (wrong account / non-Windows) must be treated as unset,
        // never surfaced as the literal ciphertext to the app.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(SECRETS_FILE);
        std::fs::write(&p, "OPENROUTER_API_KEY=dpapi:not-valid-base64-or-blob!!!\n").unwrap();
        let keys = ApiKeys::load(tmp.path());
        assert!(keys.openrouter.is_none(), "an undecryptable blob must read as unset, not as the raw string");
    }

    #[test]
    fn save_key_rejects_injection_and_unknown_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A newline in the value would inject a second KEY= line into the env file.
        assert!(ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "abc\nGEMINI_API_KEY=stolen").is_err());
        assert!(ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "has space").is_err());
        assert!(ApiKeys::save_key(dir, "NOT_A_KEY", "x").is_err());
        assert!(!dir.join(SECRETS_FILE).exists(), "a rejected save must not create/alter the file");
    }
}
