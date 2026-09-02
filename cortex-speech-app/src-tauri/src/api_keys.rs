//! Local API-key store for the supported cloud engines (Gemini and OpenRouter).
//!
//! Keys live ONLY on the user's machine: read from `{app_data_dir}/secrets.env` (`KEY=VALUE` lines)
//! or, as an override, environment variables. They are NEVER logged (the `Debug` impl masks values),
//! NEVER committed (the file is in the app data dir, outside the repo), and never leave the device
//! except in the outbound request to each provider's own API. Cortex reads them here; nothing else
//! handles them in plaintext.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// The file, inside the app data dir, the user pastes their keys into.
pub const SECRETS_FILE: &str = "secrets.env";

/// The keys Cortex looks for.
pub const KEY_NAMES: [&str; 2] = ["GEMINI_API_KEY", "OPENROUTER_API_KEY"];
pub const MAX_API_KEY_CHARS: usize = 16_384;

/// One process-wide authority for the complete secrets-file read/modify/replace transaction.
///
/// The Settings panel can legitimately issue Gemini and OpenRouter saves at the same time.  The
/// canonical file is shared, so serializing only each individual fs call is insufficient: both
/// writers could read the same old bytes, then each publish a different one-key update and report
/// success while the later replace silently erased the earlier key.  Keep the gate here, at the
/// lowest write boundary, so every current and future caller receives the same protection.
static SECRETS_WRITE_GATE: Mutex<()> = Mutex::new(());

fn lock_secrets_write() -> MutexGuard<'static, ()> {
    SECRETS_WRITE_GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// API keys, loaded from the local secrets file / environment. Optional — a provider with no key is
/// simply not used.
#[derive(Clone, Default)]
pub struct ApiKeys {
    pub gemini: Option<String>,
    pub openrouter: Option<String>,
}

impl std::fmt::Debug for ApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER print key values — only whether each is set.
        let mark = |o: &Option<String>| if o.is_some() { "set" } else { "unset" };
        f.debug_struct("ApiKeys")
            .field("gemini", &mark(&self.gemini))
            .field("openrouter", &mark(&self.openrouter))
            .finish()
    }
}

impl ApiKeys {
    /// Load keys from `{data_dir}/secrets.env`, with environment variables taking precedence; a
    /// BLANK variable counts as unset and leaves the stored key alone (see `overlay_environment`).
    ///
    /// A genuinely missing file yields all-`None`; recovery, read, and decryption failures are
    /// returned. Treating those failures as "unset" would make a crash-stranded replacement backup
    /// indistinguishable from an intentional clear and could erase the other provider on the next
    /// save. Blank values still count as unset.
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let path = data_dir.join(SECRETS_FILE);
        crate::atomic_file::recover_interrupted_replace(&path)
            .map_err(|e| format!("recover interrupted secrets-file replacement: {e}"))?;
        let mut map = parse_env_file(&path)?;
        // The process environment is layered over the store in production. The library TEST binary
        // never reads it: a shell that exports a key would otherwise hand every test a live one --
        // the import_flow tests assert none is in scope and go red, and a cloud path would upload
        // for real. Tests that exercise the overlay itself call `overlay_environment` with an
        // injected lookup. A cargo `[env]` table could have blanked the names instead, but the
        // owner-proof helper build refuses any `.cargo/config.toml` other than the registered
        // src-tauri one, whose hash is bound into the tracked contract (measured 2026-09-02: a root
        // config broke proof preparation on the release workstation). CI blanks both names in ci.yml.
        #[cfg(not(test))]
        overlay_environment(&mut map, |name| std::env::var(name).ok());
        Ok(Self { gemini: map.remove("GEMINI_API_KEY"), openrouter: map.remove("OPENROUTER_API_KEY") })
    }

    /// The provider names that have a key configured — for an honest "what's connected" status. This
    /// returns names only, NEVER the key values.
    pub fn configured_providers(&self) -> Vec<&'static str> {
        let mut providers = Vec::new();
        if self.gemini.is_some() {
            providers.push("gemini");
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
        // Hold this across recovery, read, merge, staged write, atomic replace and directory sync.
        // Shorter locking still permits a lost update between the read and the final replace.
        let _write = lock_secrets_write();
        let path = data_dir.join(SECRETS_FILE);
        // Recover BEFORE reading or deciding this is a first save. On Windows the atomic swap can be
        // interrupted after canonical -> backup but before temp -> canonical; the backup then holds
        // every provider key while `path` is absent. Falling back to the blank template in that state
        // would overwrite the only complete copy when saving one provider.
        crate::atomic_file::recover_interrupted_replace(&path)
            .map_err(|e| format!("recover interrupted secrets-file replacement: {e}"))?;
        // ONLY "the file does not exist yet" may fall back to the template. Every other read error —
        // a sharing violation from an AV scanner or the search indexer, a permission error, a
        // transient IO fault — must abort the write.
        //
        // The template carries blank placeholders for every supported provider, so treating an unreadable
        // file as an absent one rewrites secrets.env with every OTHER provider's key erased, and
        // then returns Ok(()) so the Settings UI reports the save succeeded. Saving the Gemini key
        // would silently unset OpenRouter, which surfaces later only as
        // "not configured".
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::template(),
            Err(e) => return Err(format!("read secrets file (refusing to overwrite it blind): {e}")),
        };
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
        // The shared atomic replace, not a bare rename: it fsyncs the staged bytes before the swap
        // and the directory after it. A bare rename is atomic for the NAME only — a power loss just
        // after it can expose a zero-length secrets.env, wiping every key at once.
        crate::atomic_file::replace_file(&tmp, &path).map_err(|e| format!("replace secrets file: {e}"))?;
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
    if value.chars().count() > MAX_API_KEY_CHARS {
        return Err(format!("API key is too long (max {MAX_API_KEY_CHARS} characters)"));
    }
    if value.contains(|c: char| c.is_control() || c.is_whitespace()) {
        return Err("API key contains whitespace or control characters — paste the key exactly".to_string());
    }
    Ok(value.to_string())
}

/// Lay the process environment over the parsed store. A present, non-blank variable wins; a BLANK
/// one counts as unset -- the same rule the store's own `NAME=` template lines follow -- so it
/// neither overrides nor clears a stored key. Until 2026-09-02 a blank variable REMOVED the stored
/// key: a stray `GEMINI_API_KEY=` in a shell profile silently disabled the app's key, and the test
/// isolation in the root `.cargo/config.toml` (which blanks both names for every cargo-launched
/// process) erased every store a test had built. `lookup` is injected so the rule is testable
/// without mutating the process environment under parallel tests.
fn overlay_environment(map: &mut HashMap<String, String>, lookup: impl Fn(&str) -> Option<String>) {
    for name in KEY_NAMES {
        let Some(value) = lookup(name) else { continue };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        map.insert(name.to_string(), trimmed.to_string());
    }
}

/// Parse a minimal `.env` file: `KEY=VALUE` per line; `#` comments and blank lines ignored;
/// surrounding whitespace and a matching pair of single/double quotes stripped; blank values dropped.
fn parse_env_file(path: &Path) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(map),
        Err(error) => return Err(format!("read secrets file: {error}")),
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
            // the app only ever sees plaintext. A blob that cannot be decrypted (copied to another
            // Windows account, or damaged) is an explicit load error — never surfaced as ciphertext
            // and never misreported as an intentionally unset provider.
            if crate::dpapi::is_protected(value) {
                match crate::dpapi::unprotect(value) {
                    Ok(plain) if !plain.is_empty() => {
                        map.insert(key.to_string(), plain);
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("could not decrypt DPAPI-protected {key}: {e}")),
                }
            } else {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_secrets_file_aborts_the_write_instead_of_erasing_the_other_keys() {
        // A read failure that is NOT "missing" used to fall back to the blank template, so saving one
        // provider's key rewrote the file with the other two ERASED — and returned Ok(()), so the UI
        // said "saved". A directory standing where the file belongs reproduces that class of failure
        // portably (read_to_string fails, and not with NotFound).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir(dir.join(SECRETS_FILE)).unwrap();

        let result = ApiKeys::write_key_line(dir, "GEMINI_API_KEY", "value".to_string());

        assert!(result.is_err(), "an unreadable secrets file must FAIL the save, not silently rewrite it");
        assert!(dir.join(SECRETS_FILE).is_dir(), "the existing entry must be left exactly as it was found");
    }

    #[test]
    fn a_missing_secrets_file_is_still_created_from_the_template() {
        // The other side of the same branch: first-ever save must keep working.
        let tmp = tempfile::tempdir().unwrap();
        ApiKeys::write_key_line(tmp.path(), "GEMINI_API_KEY", "abc".to_string()).unwrap();
        let written = std::fs::read_to_string(tmp.path().join(SECRETS_FILE)).unwrap();
        assert!(written.contains("GEMINI_API_KEY=abc"));
        assert!(written.contains("OPENROUTER_API_KEY="));
    }

    #[test]
    fn saving_one_key_preserves_the_others() {
        // The regression the read-error branch exists to protect, stated directly.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join(SECRETS_FILE), "GEMINI_API_KEY=gem\nLEGACY_KEY=preserved\nOPENROUTER_API_KEY=router\n")
            .unwrap();

        ApiKeys::write_key_line(dir, "GEMINI_API_KEY", "newgem".to_string()).unwrap();

        let written = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert!(written.contains("GEMINI_API_KEY=newgem"));
        assert!(written.contains("LEGACY_KEY=preserved"), "unrelated lines must survive: {written}");
        assert!(written.contains("OPENROUTER_API_KEY=router"), "other keys must survive: {written}");
    }

    #[test]
    fn concurrent_provider_writes_cannot_erase_each_other() {
        use std::sync::{Arc, Barrier};

        // Repeat the simultaneous start so this remains a meaningful characterization of the
        // read/modify/replace boundary instead of a one-off scheduling accident.
        for round in 0..32 {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            let barrier = Arc::new(Barrier::new(3));
            let gemini_barrier = Arc::clone(&barrier);
            let gemini_dir = dir.clone();
            let gemini = std::thread::spawn(move || {
                gemini_barrier.wait();
                ApiKeys::save_key(&gemini_dir, "GEMINI_API_KEY", &format!("gem-{round}"))
            });
            let router_barrier = Arc::clone(&barrier);
            let router_dir = dir.clone();
            let router = std::thread::spawn(move || {
                router_barrier.wait();
                ApiKeys::save_key(&router_dir, "OPENROUTER_API_KEY", &format!("router-{round}"))
            });

            barrier.wait();
            gemini.join().unwrap().unwrap();
            router.join().unwrap().unwrap();

            let loaded = ApiKeys::load(&dir).unwrap();
            assert_eq!(loaded.gemini.as_deref(), Some(format!("gem-{round}").as_str()));
            assert_eq!(loaded.openrouter.as_deref(), Some(format!("router-{round}").as_str()));
        }
    }

    #[test]
    fn concurrent_clear_and_update_preserve_the_unrelated_provider() {
        use std::sync::{Arc, Barrier};

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        ApiKeys::save_key(&dir, "GEMINI_API_KEY", "old-gem").unwrap();
        ApiKeys::save_key(&dir, "OPENROUTER_API_KEY", "old-router").unwrap();
        let barrier = Arc::new(Barrier::new(3));

        let clear_barrier = Arc::clone(&barrier);
        let clear_dir = dir.clone();
        let clear = std::thread::spawn(move || {
            clear_barrier.wait();
            ApiKeys::save_key(&clear_dir, "GEMINI_API_KEY", "")
        });
        let update_barrier = Arc::clone(&barrier);
        let update_dir = dir.clone();
        let update = std::thread::spawn(move || {
            update_barrier.wait();
            ApiKeys::save_key(&update_dir, "OPENROUTER_API_KEY", "new-router")
        });

        barrier.wait();
        clear.join().unwrap().unwrap();
        update.join().unwrap().unwrap();

        let loaded = ApiKeys::load(&dir).unwrap();
        assert_eq!(loaded.gemini, None);
        assert_eq!(loaded.openrouter.as_deref(), Some("new-router"));
    }

    #[test]
    fn a_blank_environment_value_is_unset_and_never_clears_the_store() {
        let stored = || HashMap::from([("GEMINI_API_KEY".to_string(), "AIzaStored".to_string())]);
        let gemini = |map: &HashMap<String, String>| map.get("GEMINI_API_KEY").cloned();

        let mut map = stored();
        overlay_environment(&mut map, |_| Some(String::new()));
        assert_eq!(gemini(&map).as_deref(), Some("AIzaStored"), "blank counts as unset, not as a removal");

        let mut map = stored();
        overlay_environment(&mut map, |_| Some("   ".to_string()));
        assert_eq!(gemini(&map).as_deref(), Some("AIzaStored"), "whitespace is blank");

        let mut map = stored();
        overlay_environment(&mut map, |_| None);
        assert_eq!(gemini(&map).as_deref(), Some("AIzaStored"), "absent leaves the store alone");

        let mut map = stored();
        overlay_environment(&mut map, |name| (name == "GEMINI_API_KEY").then(|| " AIzaEnv ".to_string()));
        assert_eq!(gemini(&map).as_deref(), Some("AIzaEnv"), "a real value overrides, trimmed");
        assert!(!map.contains_key("OPENROUTER_API_KEY"), "only names the lookup answers are touched");
    }

    #[test]
    fn parse_env_file_handles_comments_quotes_and_blanks() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.env");
        std::fs::write(
            &p,
            "# my keys\nGEMINI_API_KEY = abc123 \n\nQUOTED=\"xyz\"\nEMPTY=\n# OPENROUTER_API_KEY=nope\n",
        )
        .unwrap();
        let m = parse_env_file(&p).unwrap();
        assert_eq!(m.get("GEMINI_API_KEY").map(String::as_str), Some("abc123"), "whitespace trimmed");
        assert_eq!(m.get("QUOTED").map(String::as_str), Some("xyz"), "quotes stripped");
        assert!(!m.contains_key("EMPTY"), "blank value dropped");
        assert!(!m.contains_key("OPENROUTER_API_KEY"), "commented-out line not parsed");
    }

    #[test]
    fn parse_env_file_missing_is_empty_no_error() {
        assert!(parse_env_file(Path::new("/no/such/secrets.env")).unwrap().is_empty());
    }

    #[test]
    fn configured_providers_lists_only_set_keys_never_values() {
        let keys = ApiKeys { gemini: Some("g".into()), openrouter: Some("o".into()) };
        assert_eq!(keys.configured_providers(), vec!["gemini", "openrouter"]);
    }

    #[test]
    fn api_key_values_are_bounded_before_encryption_or_file_io() {
        let oversized = "k".repeat(MAX_API_KEY_CHARS + 1);
        let error = validate_key_value("GEMINI_API_KEY", &oversized).expect_err("oversized secret must refuse");
        assert!(error.contains("too long"));
        assert!(!error.contains(&oversized));
        assert_eq!(
            validate_key_value("GEMINI_API_KEY", &"k".repeat(MAX_API_KEY_CHARS)).unwrap().len(),
            MAX_API_KEY_CHARS
        );
    }

    #[test]
    fn debug_never_leaks_key_values() {
        let keys = ApiKeys { gemini: Some("super-secret-key".into()), openrouter: None };
        let rendered = format!("{keys:?}");
        assert!(!rendered.contains("super-secret-key"), "Debug must never print a key value");
        assert!(rendered.contains("set") && rendered.contains("unset"));
    }

    #[test]
    fn template_has_every_supported_empty_placeholder() {
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
        let keys = ApiKeys::load(dir).unwrap();
        assert_eq!(keys.openrouter.as_deref(), Some("sk-or-abc123"));
        let contents = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert!(contents.contains("GEMINI_API_KEY="), "template placeholders preserved");
        assert!(contents.starts_with('#'), "template comment preserved");

        // Update in place — other keys/lines untouched.
        ApiKeys::save_key(dir, "GEMINI_API_KEY", "g-key").unwrap();
        ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "sk-or-NEW").unwrap();
        let keys = ApiKeys::load(dir).unwrap();
        assert_eq!(keys.openrouter.as_deref(), Some("sk-or-NEW"));
        assert_eq!(keys.gemini.as_deref(), Some("g-key"));
        let contents = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert_eq!(contents.matches("OPENROUTER_API_KEY=").count(), 1, "one line per key, replaced not appended");

        // Empty value clears the key (line stays as an unset placeholder).
        ApiKeys::save_key(dir, "OPENROUTER_API_KEY", "").unwrap();
        assert!(ApiKeys::load(dir).unwrap().openrouter.is_none(), "cleared key must read back as unset");
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

        assert_eq!(ApiKeys::load(dir).unwrap().openrouter.as_deref(), Some(secret), "loads back transparently");

        // A plaintext key alongside the protected one still loads (additive, not a replacement).
        ApiKeys::save_key(dir, "GEMINI_API_KEY", "g-plain").unwrap();
        let keys = ApiKeys::load(dir).unwrap();
        assert_eq!(keys.gemini.as_deref(), Some("g-plain"));
        assert_eq!(keys.openrouter.as_deref(), Some(secret));

        // Clearing a protected key works through the same path.
        ApiKeys::save_key_protected(dir, "OPENROUTER_API_KEY", "").unwrap();
        assert!(ApiKeys::load(dir).unwrap().openrouter.is_none());
    }

    #[test]
    fn a_corrupt_dpapi_blob_is_an_explicit_load_error_never_ciphertext_or_unset() {
        // A dpapi: value that can't be decrypted (wrong account / non-Windows) is not equivalent to
        // an intentionally absent key. Surface the error without ever returning the ciphertext.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(SECRETS_FILE);
        std::fs::write(&p, "OPENROUTER_API_KEY=dpapi:not-valid-base64-or-blob!!!\n").unwrap();
        let error = ApiKeys::load(tmp.path()).expect_err("an undecryptable key must be explicit");
        assert!(error.contains("could not decrypt DPAPI-protected OPENROUTER_API_KEY"));
        assert!(!error.contains("not-valid-base64-or-blob"), "the ciphertext must not leak in the error");
    }

    #[test]
    fn load_recovers_a_crash_stranded_secrets_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(SECRETS_FILE);
        let backup = tmp.path().join(format!("{SECRETS_FILE}.replace-bak-1234"));
        std::fs::write(&backup, "GEMINI_API_KEY=gem\nOPENROUTER_API_KEY=router\n").unwrap();
        assert!(!canonical.exists(), "fixture models the gap between the Windows swap renames");

        let keys = ApiKeys::load(tmp.path()).expect("load must promote the orphaned good copy");
        assert_eq!(keys.gemini.as_deref(), Some("gem"));
        assert_eq!(keys.openrouter.as_deref(), Some("router"));
        assert!(canonical.exists() && !backup.exists(), "the backup is promoted into canonical place");
    }

    #[test]
    fn save_recovers_a_crash_stranded_backup_before_preserving_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(SECRETS_FILE);
        let backup = tmp.path().join(format!("{SECRETS_FILE}.replace-bak-5678"));
        std::fs::write(&backup, "GEMINI_API_KEY=old-gem\nOPENROUTER_API_KEY=router\n").unwrap();

        ApiKeys::save_key(tmp.path(), "GEMINI_API_KEY", "new-gem").expect("save recovers before read");
        let contents = std::fs::read_to_string(canonical).unwrap();
        assert!(contents.contains("GEMINI_API_KEY=new-gem"));
        assert!(
            contents.contains("OPENROUTER_API_KEY=router"),
            "saving one key after recovery must preserve the other provider: {contents}"
        );
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
