//! Windows DPAPI (CryptProtectData) at-rest protection for the local API-key store.
//!
//! `secrets.env` values may be stored either as plaintext (`GEMINI_API_KEY=sk-...`) or, when the
//! owner opts in, as a DPAPI blob (`GEMINI_API_KEY=dpapi:<base64>`). DPAPI ties the ciphertext to the
//! current Windows user account, so a copied `secrets.env` is useless on another machine/account. This
//! is defense-in-depth for a single-user offline app — NOT a substitute for OS account security.
//!
//! On non-Windows (the Cowork sandbox), `protect` is unavailable and `unprotect` errors: the app runs
//! on Windows; these stubs keep the crate compiling and honest elsewhere. Plaintext keys always work
//! on every platform — DPAPI is purely additive.

use base64::Engine as _;

/// Marker prefix for a DPAPI-protected value in `secrets.env`.
pub const DPAPI_PREFIX: &str = "dpapi:";

/// Is this stored value a DPAPI blob (vs plaintext)?
pub fn is_protected(value: &str) -> bool {
    value.starts_with(DPAPI_PREFIX)
}

/// Encrypt `plaintext` into a `dpapi:<base64>` string for storage. Windows-only.
#[cfg(windows)]
pub fn protect(plaintext: &str) -> Result<String, String> {
    let blob = dpapi_windows::crypt_protect(plaintext.as_bytes())?;
    Ok(format!("{DPAPI_PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(blob)))
}

/// Decrypt a `dpapi:<base64>` string produced by [`protect`]. Windows-only.
#[cfg(windows)]
pub fn unprotect(stored: &str) -> Result<String, String> {
    let b64 = stored.strip_prefix(DPAPI_PREFIX).ok_or("not a dpapi-protected value")?;
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("dpapi base64: {e}"))?;
    let bytes = dpapi_windows::crypt_unprotect(&blob)?;
    String::from_utf8(bytes).map_err(|e| format!("dpapi utf8: {e}"))
}

#[cfg(not(windows))]
pub fn protect(_plaintext: &str) -> Result<String, String> {
    Err("DPAPI protection is only available on Windows".to_string())
}

#[cfg(not(windows))]
pub fn unprotect(_stored: &str) -> Result<String, String> {
    Err("DPAPI-protected keys can only be read on Windows (the account that wrote them)".to_string())
}

#[cfg(windows)]
mod dpapi_windows {
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

    /// RAII guard: DPAPI writes output into a heap block the caller must `LocalFree`. Copies the bytes
    /// out, then frees on drop — no leak even on an early return/panic.
    struct OutBlob(CRYPT_INTEGER_BLOB);
    impl OutBlob {
        fn empty() -> Self {
            Self(CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() })
        }
        fn to_vec(&self) -> Vec<u8> {
            if self.0.pbData.is_null() || self.0.cbData == 0 {
                return Vec::new();
            }
            unsafe { std::slice::from_raw_parts(self.0.pbData, self.0.cbData as usize).to_vec() }
        }
    }
    impl Drop for OutBlob {
        fn drop(&mut self) {
            if !self.0.pbData.is_null() {
                unsafe { LocalFree(self.0.pbData as HLOCAL) };
            }
        }
    }

    fn in_blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    pub fn crypt_protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        // Input is read-only to DPAPI (pDataIn is *const), so &input — no mut.
        let input = in_blob(plaintext);
        // MUST be `&mut` + a mutable-provenance pointer: DPAPI WRITES the result struct through this
        // out-param, and writing through a pointer derived from a shared `&` is undefined behavior
        // (Stacked/Tree Borrows; Miri-flagged). &mut out.0 gives the write the provenance it needs.
        let mut out = OutBlob::empty();
        // CRYPTPROTECT_UI_FORBIDDEN (0x1): never prompt (this runs headless/background too).
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0x1,
                &mut out.0,
            )
        };
        if ok == 0 {
            return Err(format!("CryptProtectData failed (os error {})", std::io::Error::last_os_error()));
        }
        let v = out.to_vec();
        if v.is_empty() {
            return Err("CryptProtectData returned an empty blob".to_string());
        }
        Ok(v)
    }

    pub fn crypt_unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
        let input = in_blob(blob); // read-only input (pDataIn is *const)
                                   // &mut for the DPAPI-written out-param — see crypt_protect (shared-ref write = UB).
        let mut out = OutBlob::empty();
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0x1,
                &mut out.0,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptUnprotectData failed (os error {}) — wrong Windows account?",
                std::io::Error::last_os_error()
            ));
        }
        Ok(out.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_protected_detects_the_marker() {
        assert!(is_protected("dpapi:AAAA"));
        assert!(!is_protected("sk-plaintext"));
        assert!(!is_protected(""));
    }

    #[cfg(windows)]
    #[test]
    fn protect_then_unprotect_roundtrips_and_ciphertext_differs() {
        let secret = "sk-or-verysecret-ключ-کلیل";
        let stored = protect(secret).expect("protect on windows");
        assert!(is_protected(&stored));
        assert!(!stored.contains(secret), "stored blob must not contain the plaintext");
        assert_eq!(unprotect(&stored).expect("unprotect roundtrip"), secret);
    }

    #[cfg(not(windows))]
    #[test]
    fn protect_is_unavailable_off_windows() {
        assert!(protect("x").is_err());
        assert!(unprotect("dpapi:AAAA").is_err());
    }
}
